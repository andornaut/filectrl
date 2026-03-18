use std::{
    env,
    io::{Result, Stdout, stdout},
    ops::{Deref, DerefMut},
    panic,
};

use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    crossterm::{
        event::{DisableMouseCapture, EnableMouseCapture, PopKeyboardEnhancementFlags},
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    },
};

type CrosstermTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Check if the terminal supports truecolor (24-bit color)
pub fn supports_truecolor() -> bool {
    match env::var("COLORTERM") {
        Ok(val) => {
            let lower = val.to_lowercase();
            lower.contains("truecolor") || lower.contains("24bit")
        }
        Err(_) => false,
    }
}

/// A terminal wrapper that restores the terminal state on drop.
///
/// # Cleanup strategy
///
/// Two cleanup paths exist, each covering scenarios the other cannot:
///
/// 1. **`Drop`** — runs on normal exit and on panics in debug builds (which
///    unwind the stack). This is the primary path for the happy path and for
///    development-time crashes.
///
/// 2. **Panic hook** (installed in `try_new`) — runs on panics in *release*
///    builds, where `panic = "abort"` skips unwinding entirely and therefore
///    never calls `Drop`. Without the hook, a release-build panic would leave
///    the shell in raw mode / alternate screen.
///
/// In debug builds a panic triggers *both* paths (hook fires, then `Drop`
/// runs as the stack unwinds). The `cleaned_up` flag prevents the second
/// call from emitting duplicate terminal escape sequences.
pub struct CleanupOnDropTerminal {
    terminal: CrosstermTerminal,
    /// Guards against double-cleanup: set to `true` after the first call to
    /// `cleanup()` so that a subsequent call from the other path is a no-op.
    cleaned_up: bool,
}

impl CleanupOnDropTerminal {
    pub fn try_new() -> Result<Self> {
        // Release builds use `panic = "abort"`, which skips stack unwinding and
        // therefore never calls `Drop`. This hook ensures the terminal is
        // restored even in that case. In debug builds both this hook and `Drop`
        // will fire on a panic; `cleaned_up` prevents the second from being a
        // problem.
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            ratatui::restore();
            original_hook(info);
        }));

        enable_raw_mode()?;

        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        terminal.hide_cursor()?;
        terminal.clear()?;
        Ok(Self {
            terminal,
            cleaned_up: false,
        })
    }

    fn cleanup(&mut self) {
        if self.cleaned_up {
            return;
        }
        self.cleaned_up = true;
        // Ignore errors during cleanup — we're already in a failure/exit state
        // and there is nothing useful to do with them.
        let _ = self.terminal.show_cursor();
        let _ = execute!(
            self.terminal.backend_mut(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            LeaveAlternateScreen,
        );
        let _ = disable_raw_mode();
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
        self.cleanup();
    }
}
