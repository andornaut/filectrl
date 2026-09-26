use std::{
    fs::{File, OpenOptions},
    io::{Result, Stdout, stdin, stdout},
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
    os::fd::AsFd,
    panic,
    sync::atomic::{AtomicBool, Ordering},
};

use nix::{
    sys::termios::{SetArg, Termios, tcgetattr, tcsetattr},
    unistd::{getpgrp, tcgetpgrp},
};

use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    crossterm::{
        cursor::Show,
        event::{
            DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
            KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
};

type CrosstermTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Whether `$COLORTERM` indicates truecolor.
pub fn supports_truecolor(colorterm: Option<&str>) -> bool {
    colorterm.is_some_and(|value| {
        let lower = value.to_lowercase();
        lower.contains("truecolor") || lower.contains("24bit")
    })
}

/// Process-wide "already restored" guard, so only the first cleanup path
/// restores: a second `PopKeyboardEnhancementFlags` would pop the main
/// screen's stack. Static because the panic hook cannot reach instance state.
static TERMINAL_RESTORED: AtomicBool = AtomicBool::new(false);

/// Undoes everything `try_new` set up, at most once per acquisition. Errors
/// are ignored: this runs on exit and panic paths.
fn restore_terminal_once() {
    if TERMINAL_RESTORED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = execute!(
        stdout(),
        Show,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
    );
    let _ = disable_raw_mode();
}

/// A terminal wrapper that restores the terminal state on drop.
///
/// `Drop` covers a normal exit and the panic hook covers a panic, since
/// `panic = "abort"` never runs `Drop`.
pub struct CleanupOnDropTerminal {
    /// Dropped by hand, so ratatui's drop is skipped when the cursor cannot be
    /// shown (see `Drop`).
    terminal: ManuallyDrop<CrosstermTerminal>,
    /// The shell's terminal settings, read before raw mode and restored before
    /// raw mode is entered again, since a foreground program may change them.
    /// `None` when they could not be read.
    shell_settings: Option<Termios>,
}

/// Opens the controlling terminal, which raw mode applies to.
pub(super) fn controlling_terminal() -> Result<File> {
    OpenOptions::new().read(true).write(true).open("/dev/tty")
}

/// Runs `act` on the controlling terminal, or on stdin if it cannot be opened.
fn on_terminal<T>(act: impl FnOnce(std::os::fd::BorrowedFd<'_>) -> T) -> T {
    match controlling_terminal() {
        Ok(tty) => act(tty.as_fd()),
        Err(_) => act(stdin().as_fd()),
    }
}

impl CleanupOnDropTerminal {
    pub fn try_new() -> Result<Self> {
        TERMINAL_RESTORED.store(false, Ordering::SeqCst);

        // `panic = "abort"` never runs `Drop`, so the hook restores the terminal.
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore_terminal_once();
            original_hook(info);
        }));

        let shell_settings = on_terminal(|fd| tcgetattr(fd).ok());
        enable_raw_mode()?;

        // A failure from here on must roll back: no instance exists yet for `Drop`.
        let build = || -> Result<Self> {
            let mut stdout = stdout();
            enter_interface_modes(&mut stdout)?;
            let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
            terminal.hide_cursor()?;
            terminal.clear()?;
            Ok(Self {
                terminal: ManuallyDrop::new(terminal),
                shell_settings,
            })
        };
        build().inspect_err(|_| restore_terminal_once())
    }

    /// Hands the terminal back as the shell left it, for a foreground program.
    /// Marked restored, so a panic meanwhile does not undo it again.
    pub fn suspend(&mut self) {
        // So ratatui's drop has no cursor to show on a terminal that hung up.
        let _ = self.terminal.show_cursor();
        restore_terminal_once();
    }

    /// Restores the shell's settings on a suspended terminal, ignoring failure:
    /// the process is quitting.
    pub fn release(&mut self) {
        if let Some(settings) = &self.shell_settings {
            let _ = on_terminal(|fd| release_settings(fd, settings));
        }
    }

    /// Takes the terminal back after `suspend` and clears it so the next draw
    /// repaints every cell. A failure is rolled back like one in `try_new`.
    pub fn resume(&mut self) -> Result<()> {
        let settings = self.shell_settings.clone();
        take_back(
            || match &settings {
                Some(settings) => on_terminal(|fd| restore_settings(fd, settings)),
                None => Ok(()),
            },
            enable_raw_mode,
        )?;
        let mut resume = || -> Result<()> {
            enter_interface_modes(self.terminal.backend_mut())?;
            self.terminal.hide_cursor()?;
            self.terminal.clear()
        };
        resume().inspect_err(|_| restore_terminal_once())
    }
}

/// The first half of `resume`: the shell's settings go back before raw mode
/// records them as the settings to restore.
fn take_back(
    restore_settings: impl FnOnce() -> Result<()>,
    enable_raw_mode: impl FnOnce() -> Result<()>,
) -> Result<()> {
    restore_settings()?;
    enable_raw_mode()?;
    TERMINAL_RESTORED.store(false, Ordering::SeqCst);
    Ok(())
}

/// `restore_settings`, only while this process group owns the terminal. From
/// the background the write raises SIGTTOU and would clobber another job.
fn release_settings(fd: impl AsFd, settings: &Termios) -> Result<()> {
    if tcgetpgrp(&fd) != Ok(getpgrp()) {
        return Ok(());
    }
    restore_settings(fd, settings)
}

/// Puts `settings` back on the terminal `fd`.
fn restore_settings(fd: impl AsFd, settings: &Termios) -> Result<()> {
    tcsetattr(fd, SetArg::TCSANOW, settings).map_err(std::io::Error::from)
}

fn enter_interface_modes(out: &mut impl std::io::Write) -> Result<()> {
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        // Without it a pasted line break arrives as Enter.
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    )
}

impl Deref for CleanupOnDropTerminal {
    type Target = CrosstermTerminal;

    fn deref(&self) -> &CrosstermTerminal {
        &self.terminal
    }
}

impl DerefMut for CleanupOnDropTerminal {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl Drop for CleanupOnDropTerminal {
    fn drop(&mut self) {
        // On a terminal that hung up ratatui's drop fails to show the cursor and
        // panics in `eprintln!`, aborting. So the cursor is shown here, and ratatui's
        // drop is skipped when that fails.
        let shown = self.terminal.show_cursor().is_ok();
        restore_terminal_once();
        if shown {
            // SAFETY: `self.terminal` is not used again, since this is `drop`.
            #[allow(unsafe_code)]
            unsafe {
                ManuallyDrop::drop(&mut self.terminal);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::{release_settings, restore_settings, supports_truecolor, take_back};

    #[test_case(Some("truecolor") => true ; "truecolor")]
    #[test_case(Some("24bit") => true ; "24bit")]
    #[test_case(Some("TrueColor") => true ; "any case")]
    #[test_case(Some("yes") => false ; "another value")]
    #[test_case(None => false ; "unset")]
    fn colorterm_decides_truecolor(colorterm: Option<&str>) -> bool {
        supports_truecolor(colorterm)
    }

    /// Settings a program leaves (echo off) are replaced by those read at startup.
    #[test]
    fn the_shell_settings_replace_what_a_program_left() {
        use nix::{
            pty::openpty,
            sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr},
        };

        let pty = openpty(None, None).unwrap();
        let shell = tcgetattr(&pty.slave).unwrap();
        assert!(shell.local_flags.contains(LocalFlags::ECHO));
        let mut left = shell.clone();
        left.local_flags.remove(LocalFlags::ECHO);
        tcsetattr(&pty.slave, SetArg::TCSANOW, &left).unwrap();

        restore_settings(&pty.slave, &shell).unwrap();

        assert!(
            tcgetattr(&pty.slave)
                .unwrap()
                .local_flags
                .contains(LocalFlags::ECHO)
        );
    }

    /// A terminal this process group does not own is left alone. A pty that is not
    /// the test's controlling terminal stands in for one.
    #[test]
    fn a_terminal_owned_by_another_group_keeps_its_settings() {
        use nix::{
            pty::openpty,
            sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr},
        };

        let pty = openpty(None, None).unwrap();
        let shell = tcgetattr(&pty.slave).unwrap();
        let mut left = shell.clone();
        left.local_flags.remove(LocalFlags::ECHO);
        tcsetattr(&pty.slave, SetArg::TCSANOW, &left).unwrap();

        release_settings(&pty.slave, &shell).unwrap();

        assert!(
            !tcgetattr(&pty.slave)
                .unwrap()
                .local_flags
                .contains(LocalFlags::ECHO)
        );
    }

    #[test]
    fn the_shell_settings_go_back_before_raw_mode() {
        let order = std::cell::RefCell::new(Vec::new());

        take_back(
            || {
                order.borrow_mut().push("settings");
                Ok(())
            },
            || {
                order.borrow_mut().push("raw mode");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(vec!["settings", "raw mode"], order.into_inner());
    }
}
