//! Programs that take the terminal over while they run (editor, pager).

use std::{
    ffi::OsString,
    io,
    path::Path,
    process::{Command, ExitStatus},
};

use anyhow::{Result, anyhow};

use super::{
    events::{ReaderGate, set_foreground_child, unblock_in_child},
    terminal::CleanupOnDropTerminal,
};
use crate::command::ForegroundProgram;

/// The command line for `program` on `path`: the first set, non-blank
/// environment variable, split into shell words, plus the path. `env` is
/// injected for tests.
pub(super) fn argv(
    env: impl Fn(&str) -> Option<OsString>,
    program: ForegroundProgram,
    path: &Path,
) -> Result<Vec<OsString>> {
    let (variables, fallback): (&[&str], &str) = match program {
        ForegroundProgram::Editor => (&["VISUAL", "EDITOR"], "vi"),
        ForegroundProgram::Pager => (&["PAGER"], "less"),
    };
    let chosen = variables.iter().find_map(|&name| {
        let value = env(name)?;
        (!value.to_string_lossy().trim().is_empty()).then_some((name, value))
    });
    let mut argv: Vec<OsString> = match chosen {
        None => vec![fallback.into()],
        Some((name, value)) => {
            let value = value
                .into_string()
                .map_err(|_| anyhow!("Cannot run ${name}: it is not valid UTF-8"))?;
            let words = shell_words::split(&value)
                .map_err(|_| anyhow!("Cannot run ${name}: its quoting is not closed"))?;
            // A comment-only value has no words; the path would then run as the program.
            if words.is_empty() {
                return Err(anyhow!("Cannot run ${name}: it names no program"));
            }
            words.into_iter().map(OsString::from).collect()
        }
    };
    argv.push(path.as_os_str().to_os_string());
    Ok(argv)
}

/// Runs `argv` in the terminal's foreground and waits for it, with the reader
/// stopped (so the program reads the keyboard alone) and the terminal in the
/// shell's modes, then takes the terminal back. The outer error is a terminal
/// that could not be taken back; the inner result is the program's.
pub(super) fn run(
    terminal: &mut CleanupOnDropTerminal,
    gate: &ReaderGate,
    argv: &[OsString],
) -> io::Result<io::Result<ExitStatus>> {
    gate.pause();
    // Covers cooked mode on both sides, so Ctrl+C is never taken as a quit.
    set_foreground_child(true);
    terminal.suspend();
    // `argv` holds at least the program and the path.
    let status = unblock_in_child(Command::new(&argv[0]).args(&argv[1..])).status();
    let resumed = terminal.resume();
    set_foreground_child(false);
    gate.resume();
    resumed.map(|()| status)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, ffi::OsString, path::Path};

    use test_case::test_case;

    use super::argv;
    use crate::command::ForegroundProgram::{self, Editor, Pager};

    fn run_argv(vars: &[(&str, &str)], program: ForegroundProgram) -> Vec<String> {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(name, value)| ((*name).to_string(), OsString::from(value)))
            .collect();
        argv(
            |name| vars.get(name).cloned(),
            program,
            Path::new("/a b/f'x"),
        )
        .unwrap()
        .into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect()
    }

    #[test_case(&[("VISUAL", "code -w"), ("EDITOR", "nano")], Editor => vec!["code", "-w", "/a b/f'x"] ; "visual comes first")]
    #[test_case(&[("EDITOR", "nano")], Editor => vec!["nano", "/a b/f'x"] ; "then editor")]
    #[test_case(&[("VISUAL", " "), ("EDITOR", "nano")], Editor => vec!["nano", "/a b/f'x"] ; "a blank variable counts as unset")]
    #[test_case(&[], Editor => vec!["vi", "/a b/f'x"] ; "vi without either")]
    #[test_case(&[("PAGER", "less -R")], Pager => vec!["less", "-R", "/a b/f'x"] ; "the pager's own variable")]
    #[test_case(&[("EDITOR", "nano")], Pager => vec!["less", "/a b/f'x"] ; "the pager ignores the editor")]
    #[test_case(&[("EDITOR", "'my editor' --wait")], Editor => vec!["my editor", "--wait", "/a b/f'x"] ; "quoted words are one argument")]
    fn the_command_line_is_the_variable_split_then_the_path(
        vars: &[(&str, &str)],
        program: ForegroundProgram,
    ) -> Vec<String> {
        run_argv(vars, program)
    }

    #[test]
    fn a_variable_with_unclosed_quoting_is_refused() {
        let error = argv(
            |name| (name == "EDITOR").then(|| OsString::from("vim 'x")),
            Editor,
            Path::new("/f"),
        )
        .unwrap_err()
        .to_string();

        assert_eq!("Cannot run $EDITOR: its quoting is not closed", error);
    }

    #[test_case::test_case("#vim" ; "a comment")]
    #[test_case::test_case(" # x" ; "a comment after spaces")]
    fn a_variable_naming_no_program_is_refused(value: &str) {
        let error = argv(
            |name| (name == "EDITOR").then(|| OsString::from(value)),
            Editor,
            Path::new("/f"),
        )
        .unwrap_err()
        .to_string();

        assert_eq!("Cannot run $EDITOR: it names no program", error);
    }
}
