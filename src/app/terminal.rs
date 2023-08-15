use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    io::{stdout, Result, Stdout},
    ops::{Deref, DerefMut},
};

type CrosstermTerminal = Terminal<CrosstermBackend<Stdout>>;

pub struct CleanupOnDropTerminal(CrosstermTerminal);

impl CleanupOnDropTerminal {
    pub fn try_new() -> Result<Self> {
        enable_raw_mode()?;

        let mut stdout = stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            // Is only supported on some terminals
            // https://docs.rs/crossterm/latest/crossterm/event/struct.PushKeyboardEnhancementFlags.html
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;

        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        terminal.hide_cursor()?;
        terminal.clear()?;
        Ok(Self(terminal))
    }

    pub fn cleanup(&mut self) {
        self.show_cursor().unwrap();
        execute!(
            self.backend_mut(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            LeaveAlternateScreen,
        )
        .unwrap();
        disable_raw_mode().unwrap();
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
