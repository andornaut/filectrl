use std::{
    io::{Result, Stdout, stdout},
    mem::ManuallyDrop,
    ops::{Deref, DerefMut},
    panic,
    sync::atomic::{AtomicBool, Ordering},
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
            })
        };
        build().inspect_err(|_| restore_terminal_once())
    }

    /// Hands the terminal back to the shell's modes, for a foreground program.
    /// Marked restored, so a panic meanwhile does not undo it again.
    pub fn suspend(&mut self) {
        restore_terminal_once();
    }

    /// Takes the terminal back after `suspend` and clears it so the next draw
    /// repaints every cell. A failure is rolled back like one in `try_new`.
    pub fn resume(&mut self) -> Result<()> {
        enable_raw_mode()?;
        TERMINAL_RESTORED.store(false, Ordering::SeqCst);
        let mut resume = || -> Result<()> {
            enter_interface_modes(self.terminal.backend_mut())?;
            self.terminal.hide_cursor()?;
            self.terminal.clear()
        };
        resume().inspect_err(|_| restore_terminal_once())
    }
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

    use super::supports_truecolor;

    #[test_case(Some("truecolor") => true ; "truecolor")]
    #[test_case(Some("24bit") => true ; "24bit")]
    #[test_case(Some("TrueColor") => true ; "any case")]
    #[test_case(Some("yes") => false ; "another value")]
    #[test_case(None => false ; "unset")]
    fn colorterm_decides_truecolor(colorterm: Option<&str>) -> bool {
        supports_truecolor(colorterm)
    }
}
