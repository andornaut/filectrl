use std::{
    fs::{File, OpenOptions},
    io::{Result, Stdout, stdin, stdout},
    ops::{Deref, DerefMut},
    os::fd::AsFd,
    panic,
    sync::atomic::{AtomicBool, Ordering},
};

use nix::sys::termios::{SetArg, Termios, tcgetattr, tcsetattr};

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

/// Whether `$COLORTERM`'s value says the terminal supports truecolor (24-bit
/// color). Unset, or not valid UTF-8, means it does not.
pub fn supports_truecolor(colorterm: Option<&str>) -> bool {
    colorterm.is_some_and(|value| {
        let lower = value.to_lowercase();
        lower.contains("truecolor") || lower.contains("24bit")
    })
}

/// Process-wide "already restored" guard: whichever cleanup path runs first
/// restores, the rest are no-ops. A second `PopKeyboardEnhancementFlags` after
/// leaving the alternate screen would pop an entry from the main screen's stack
/// that this program never pushed. Static because the panic hook is a `'static`
/// closure that cannot reach instance state; `try_new` re-arms it.
static TERMINAL_RESTORED: AtomicBool = AtomicBool::new(false);

/// Undoes everything `try_new` set up, at most once per acquisition (see
/// `TERMINAL_RESTORED`), in one shared sequence so the cleanup paths cannot
/// drift. Errors are ignored: this runs in exit and panic paths where there is
/// nothing useful to do with them.
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
/// Two cleanup paths, each covering what the other cannot: `Drop` runs on normal
/// exit, and the panic hook (installed in `try_new`) covers a panic, where
/// `panic = "abort"` never calls `Drop` and would leave the shell in raw mode.
/// `TERMINAL_RESTORED` leaves whichever runs first the only one to emit escape
/// sequences.
pub struct CleanupOnDropTerminal {
    terminal: CrosstermTerminal,
    /// The terminal's settings as the shell left them, read before raw mode.
    /// A foreground program can exit with other settings (killed with echo
    /// off), and raw mode records whatever it finds as the settings to restore
    /// on exit, so these are put back before raw mode is entered again. `None`
    /// when neither the controlling terminal nor stdin could be read.
    shell_settings: Option<Termios>,
}

/// Opens the controlling terminal, which is what raw mode applies to whether
/// or not stdin is a terminal.
pub(super) fn controlling_terminal() -> Result<File> {
    OpenOptions::new().read(true).write(true).open("/dev/tty")
}

/// Runs `act` on the controlling terminal, or on stdin when it cannot be
/// opened.
fn on_terminal<T>(act: impl FnOnce(std::os::fd::BorrowedFd<'_>) -> T) -> T {
    match controlling_terminal() {
        Ok(tty) => act(tty.as_fd()),
        Err(_) => act(stdin().as_fd()),
    }
}

impl CleanupOnDropTerminal {
    pub fn try_new() -> Result<Self> {
        // Re-arm the process-wide guard for this acquisition, so the type is
        // not silently single-use.
        TERMINAL_RESTORED.store(false, Ordering::SeqCst);

        // Every build profile but the test one uses `panic = "abort"`, which
        // skips stack unwinding and therefore never calls `Drop`. This hook
        // restores the terminal before the abort.
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore_terminal_once();
            original_hook(info);
        }));

        let shell_settings = on_terminal(|fd| tcgetattr(fd).ok());
        enable_raw_mode()?;

        // Any failure past this point must roll back what is already set up:
        // no instance exists yet for `Drop`, and without a panic the hook
        // never fires, so an early `?` would leave the shell in raw mode.
        let build = || -> Result<Self> {
            let mut stdout = stdout();
            enter_interface_modes(&mut stdout)?;
            let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
            terminal.hide_cursor()?;
            terminal.clear()?;
            Ok(Self {
                terminal,
                shell_settings,
            })
        };
        build().inspect_err(|_| restore_terminal_once())
    }

    /// Hands the terminal back in the state the shell left it, for a program
    /// that runs in the foreground. A suspended terminal counts as restored,
    /// so a panic meanwhile does not undo the modes a second time.
    pub fn suspend(&mut self) {
        // Recorded as shown while the terminal is live, so ratatui's own drop
        // has no cursor to show on a terminal that hung up meanwhile.
        let _ = self.terminal.show_cursor();
        restore_terminal_once();
    }

    /// Puts the shell's settings back on a suspended terminal that is not
    /// being taken back, ignoring a failure: the process is quitting.
    pub fn release(&mut self) {
        if let Some(settings) = &self.shell_settings {
            let _ = on_terminal(|fd| restore_settings(fd, settings));
        }
    }

    /// Takes the terminal back after `suspend` and clears it, so the next draw
    /// repaints every cell rather than only those it believes changed. A
    /// failure is rolled back like one in `try_new`.
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

/// The first half of `resume`: the shell's settings go back before raw mode is
/// entered, since raw mode records what it finds as the settings to restore on
/// exit. Once raw mode is on there is something for a cleanup path to undo.
fn take_back(
    restore_settings: impl FnOnce() -> Result<()>,
    enable_raw_mode: impl FnOnce() -> Result<()>,
) -> Result<()> {
    restore_settings()?;
    enable_raw_mode()?;
    TERMINAL_RESTORED.store(false, Ordering::SeqCst);
    Ok(())
}

/// Puts `settings` back on the terminal `fd`, so the next raw mode records
/// them rather than whatever a foreground program left behind.
fn restore_settings(fd: impl AsFd, settings: &Termios) -> Result<()> {
    tcsetattr(fd, SetArg::TCSANOW, settings).map_err(std::io::Error::from)
}

/// The modes the interface runs in, which `restore_terminal_once` undoes.
fn enter_interface_modes(out: &mut impl std::io::Write) -> Result<()> {
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        // Without it a paste arrives as keystrokes, and a line break in it as
        // Enter, so pasted text could answer prompts and run bindings.
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
        restore_terminal_once();
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::{restore_settings, supports_truecolor, take_back};

    #[test_case(Some("truecolor") => true ; "truecolor")]
    #[test_case(Some("24bit") => true ; "24bit")]
    #[test_case(Some("TrueColor") => true ; "any case")]
    #[test_case(Some("yes") => false ; "another value")]
    #[test_case(None => false ; "unset")]
    fn colorterm_decides_truecolor(colorterm: Option<&str>) -> bool {
        supports_truecolor(colorterm)
    }

    /// A program that exits with echo off leaves the terminal that way; the
    /// settings read at startup are what come back.
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

    /// Raw mode records the settings it finds, so the shell's go back first.
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
