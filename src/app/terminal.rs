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
        Ok(val) => val.to_lowercase().contains("truecolor") || val.to_lowercase().contains("24bit"),
        Err(_) => false,
    }
}

pub struct CleanupOnDropTerminal(CrosstermTerminal);

impl CleanupOnDropTerminal {
    pub fn try_new() -> Result<Self> {
        // Install a panic hook so the terminal is restored even if the app panics,
        // leaving the shell in a usable state.
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
        Ok(Self(terminal))
    }

    fn cleanup(&mut self) {
        // Ignore errors during cleanup, because we're already in a failure state
        let _ = self.show_cursor();
        let _ = execute!(
            self.backend_mut(),
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
        &self.0
    }
}

impl DerefMut for CleanupOnDropTerminal {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for CleanupOnDropTerminal {
    fn drop(&mut self) {
        self.cleanup();
    }
}
