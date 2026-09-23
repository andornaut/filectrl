use std::{
    io::{Result, Stdout, stdout},
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
/// exit and on a debug-build panic, which unwinds; the panic hook (installed in
/// `try_new`) covers a release-build panic, where `panic = "abort"` never calls
/// `Drop` and would leave the shell in raw mode.
///
/// A debug panic fires both, and `TERMINAL_RESTORED` leaves whichever runs first
/// the only one to emit escape sequences.
pub struct CleanupOnDropTerminal {
    terminal: CrosstermTerminal,
}

impl CleanupOnDropTerminal {
    pub fn try_new() -> Result<Self> {
        // Re-arm the process-wide guard for this acquisition, so the type is
        // not silently single-use.
        TERMINAL_RESTORED.store(false, Ordering::SeqCst);

        // Release builds use `panic = "abort"`, which skips stack unwinding and
        // therefore never calls `Drop`. This hook ensures the terminal is
        // restored even in that case.
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore_terminal_once();
            original_hook(info);
        }));

        enable_raw_mode()?;

        // Any failure past this point must roll back what is already set up:
        // no instance exists yet for `Drop`, and without a panic the hook
        // never fires, so an early `?` would leave the shell in raw mode.
        let build = || -> Result<Self> {
            let mut stdout = stdout();
            execute!(
                stdout,
                EnterAlternateScreen,
                EnableMouseCapture,
                // Without it a paste arrives as keystrokes, and a line break in
                // it as Enter, so pasted text could answer prompts and run
                // bindings.
                EnableBracketedPaste,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            )?;

            let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
            terminal.hide_cursor()?;
            terminal.clear()?;
            Ok(Self { terminal })
        };
        build().inspect_err(|_| restore_terminal_once())
    }
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
