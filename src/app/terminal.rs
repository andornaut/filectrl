use std::{
    io::{stdout, Result, Stdout},
    ops::{Deref, DerefMut},
};

use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        event::{DisableMouseCapture, EnableMouseCapture, PopKeyboardEnhancementFlags},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    },
    Terminal,
};

type CrosstermTerminal = Terminal<CrosstermBackend<Stdout>>;

pub struct CleanupOnDropTerminal(CrosstermTerminal);

impl CleanupOnDropTerminal {
    pub fn try_new() -> Result<Self> {
        enable_raw_mode()?;

        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture,)?;

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
