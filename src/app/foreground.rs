//! Programs that take the terminal over while they run: the editor and the
//! pager, opened on the cursor's entry.

use std::{
    ffi::OsString,
    fs::File,
    io::{self, IsTerminal},
    path::Path,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Result, anyhow};
use nix::sys::signal::{Signal, raise};

use super::{
    events::{ReaderGate, run_foreground_child, set_foreground_child},
    terminal::CleanupOnDropTerminal,
    terminal::controlling_terminal,
};
use crate::command::ForegroundProgram;

/// How long to wait for the reader thread to stop before giving up on running
/// the program. It stops as soon as its poll returns, which the wake-up in
/// `run` makes immediate; its own poll timeout (2 s) bounds the wait if the
/// wake-up is lost.
pub(super) const READER_PAUSE_TIMEOUT: Duration = Duration::from_millis(2500);

/// The command line for `program` on `path`: the first of its environment
/// variables that is set and not blank, split into words the way a shell would
/// split it, with the path appended as an argument of its own. `env` reads a
/// variable, so the order can be tested without the process environment.
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
            shell_words::split(&value)
                .map_err(|_| anyhow!("Cannot run ${name}: its quoting is not closed"))?
                .into_iter()
                .map(OsString::from)
                .collect()
        }
    };
    argv.push(path.as_os_str().to_os_string());
    Ok(argv)
}

/// What the program reads, when not this process's own stdin: the terminal,
/// if stdin is something else (`filectrl </dev/null`). The interface reads the
/// terminal whatever stdin is, and so must a program that takes it over. `None`
/// keeps stdin, including when the terminal cannot be opened.
fn child_stdin(
    stdin_is_terminal: bool,
    open_terminal: impl FnOnce() -> io::Result<File>,
) -> Option<File> {
    if stdin_is_terminal {
        return None;
    }
    open_terminal().ok()
}

/// A terminal that can be handed to another program and taken back.
pub(super) trait Handover {
    fn suspend(&mut self);

    fn resume(&mut self) -> io::Result<()>;

    /// Leaves a suspended terminal with the settings the shell had, over any a
    /// program killed part way left (echo off, raw mode). Best effort: the
    /// process is quitting, and a terminal that hung up takes no settings.
    fn release(&mut self);
}

impl Handover for CleanupOnDropTerminal {
    fn suspend(&mut self) {
        CleanupOnDropTerminal::suspend(self);
    }

    fn release(&mut self) {
        CleanupOnDropTerminal::release(self);
    }

    fn resume(&mut self) -> io::Result<()> {
        CleanupOnDropTerminal::resume(self)
    }
}

/// What became of a request to run a program in the foreground.
#[derive(Debug)]
pub(super) enum Outcome {
    /// It ran, or failed to start, with this result.
    Ran(io::Result<ExitStatus>),
    /// The reader thread did not stop in time, so the program was not run: it
    /// would have shared the keyboard with it.
    ReaderBusy,
    /// A termination signal arrived before the program started or while it
    /// ran. The terminal is left handed back for the quit that follows, with
    /// the shell's settings put back over any the program left: taking it back
    /// would only mean leaving it again, and a terminal that hung up cannot be
    /// taken back at all.
    Quit,
}

/// Runs `argv` in the foreground of the terminal and waits for it, with the
/// interface suspended: the reader thread stopped, so the program reads the
/// keyboard alone, and the terminal back in the modes the shell left it in.
/// The terminal is taken back and cleared whatever the program did.
///
/// The error is a terminal that could not be taken back, which leaves nothing
/// to draw on.
pub(super) fn run(
    terminal: &mut impl Handover,
    gate: &ReaderGate,
    pause_timeout: Duration,
    quit_requested: &dyn Fn() -> bool,
    argv: &[OsString],
) -> Result<Outcome> {
    let Some((program, arguments)) = argv.split_first() else {
        return Ok(Outcome::Ran(Err(io::Error::other("the command is empty"))));
    };
    if quit_requested() {
        return Ok(Outcome::Quit);
    }
    // crossterm answers SIGWINCH by ending its poll with a resize event, so the
    // reader reaches its checkpoint now rather than at its next timeout. The
    // request comes first, so the checkpoint the wake-up reaches is the one
    // that stops.
    gate.request_pause();
    let _ = raise(Signal::SIGWINCH);
    if !gate.wait_paused(pause_timeout) {
        gate.resume();
        return Ok(Outcome::ReaderBusy);
    }
    // Set for the whole time the terminal is the program's, cooked mode on
    // both sides of it included, so Ctrl+C there is never taken as a quit.
    set_foreground_child(true);
    terminal.suspend();
    let mut command = std::process::Command::new(program);
    command.args(arguments);
    if let Some(tty) = child_stdin(io::stdin().is_terminal(), controlling_terminal) {
        command.stdin(Stdio::from(tty));
    }
    let status = run_foreground_child(&mut command, quit_requested);
    if quit_requested() {
        terminal.release();
        return Ok(Outcome::Quit);
    }
    let resumed = terminal.resume();
    set_foreground_child(false);
    gate.resume();
    resumed?;
    Ok(Outcome::Ran(status))
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

    /// A terminal that records whether a foreground program counted as
    /// running at each step of the handover.
    #[derive(Default)]
    struct Recorded {
        child_flag_at: Vec<(&'static str, bool)>,
    }

    impl super::Handover for Recorded {
        fn suspend(&mut self) {
            self.child_flag_at
                .push(("suspend", crate::app::events::is_foreground_child()));
        }

        fn resume(&mut self) -> std::io::Result<()> {
            self.child_flag_at
                .push(("resume", crate::app::events::is_foreground_child()));
            Ok(())
        }

        fn release(&mut self) {
            self.child_flag_at
                .push(("release", crate::app::events::is_foreground_child()));
        }
    }

    #[test]
    fn a_program_is_not_run_while_the_reader_still_reads() {
        let gate = crate::app::events::ReaderGate::default();
        let mut terminal = Recorded::default();

        let outcome = super::run(
            &mut terminal,
            &gate,
            std::time::Duration::from_millis(10),
            &|| false,
            &["true".into()],
        )
        .unwrap();

        assert!(matches!(outcome, super::Outcome::ReaderBusy), "{outcome:?}");
        assert!(
            terminal.child_flag_at.is_empty(),
            "the terminal was handed over"
        );
        assert!(gate.is_open(), "the reader was left stopped");
    }

    /// Ctrl+C is the program's from before the terminal is handed over until
    /// after it is taken back.
    #[test]
    fn the_terminal_is_the_programs_across_the_whole_handover() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let _serial = crate::app::events::SIGNAL_ACTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let gate = Arc::new(crate::app::events::ReaderGate::default());
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let (gate, done) = (gate.clone(), done.clone());
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    gate.checkpoint();
                    std::thread::yield_now();
                }
            })
        };
        let mut terminal = Recorded::default();

        let outcome = super::run(
            &mut terminal,
            &gate,
            std::time::Duration::from_secs(5),
            &|| false,
            &["true".into()],
        )
        .unwrap();
        done.store(true, Ordering::SeqCst);
        reader.join().unwrap();

        assert!(
            matches!(&outcome, super::Outcome::Ran(Ok(status)) if status.success()),
            "{outcome:?}"
        );
        assert_eq!(
            vec![("suspend", true), ("resume", true)],
            terminal.child_flag_at
        );
        assert!(!crate::app::events::is_foreground_child());
    }

    #[test]
    fn a_program_reads_the_terminal_only_when_stdin_is_something_else() {
        let terminal = || std::fs::File::open("/dev/null");

        assert!(super::child_stdin(true, terminal).is_none());
        assert!(super::child_stdin(false, terminal).is_some());
        assert!(super::child_stdin(false, || Err(std::io::Error::other("no terminal"))).is_none());
    }

    #[test]
    fn nothing_is_handed_over_after_a_quit() {
        let gate = crate::app::events::ReaderGate::default();
        let mut terminal = Recorded::default();

        let outcome = super::run(
            &mut terminal,
            &gate,
            std::time::Duration::from_millis(10),
            &|| true,
            &["true".into()],
        )
        .unwrap();

        assert!(matches!(outcome, super::Outcome::Quit), "{outcome:?}");
        assert!(terminal.child_flag_at.is_empty());
    }

    /// A quit while the program ran leaves the terminal handed back, with the
    /// shell's settings over whatever the program left: the process is about
    /// to exit, and a terminal that hung up cannot be taken back.
    #[test]
    fn a_quit_during_the_program_leaves_the_terminal_to_the_shell() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };

        let _serial = crate::app::events::SIGNAL_ACTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let gate = Arc::new(crate::app::events::ReaderGate::default());
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let (gate, done) = (gate.clone(), done.clone());
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    gate.checkpoint();
                    std::thread::yield_now();
                }
            })
        };
        let mut terminal = Recorded::default();
        // Checked before the handover, before the start, once the pid is
        // known, and after the program: only the last finds a quit.
        let checks = AtomicUsize::new(0);
        let quit_requested = || checks.fetch_add(1, Ordering::SeqCst) >= 3;

        let outcome = super::run(
            &mut terminal,
            &gate,
            std::time::Duration::from_secs(5),
            &quit_requested,
            &["true".into()],
        )
        .unwrap();
        gate.resume();
        done.store(true, Ordering::SeqCst);
        reader.join().unwrap();
        crate::app::events::set_foreground_child(false);

        assert!(matches!(outcome, super::Outcome::Quit), "{outcome:?}");
        assert_eq!(
            vec![("suspend", true), ("release", true)],
            terminal.child_flag_at
        );
    }
}
