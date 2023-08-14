use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
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
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        terminal.clear()?;
        terminal.hide_cursor()?;

        Ok(Self(terminal))
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
        disable_raw_mode().unwrap();

        execute!(
            self.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )
        .unwrap();
        self.show_cursor().unwrap()
    }
}
