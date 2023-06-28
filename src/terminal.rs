use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{stdout, Result, Stdout};

type CrosstermTerminal = Terminal<CrosstermBackend<Stdout>>;

pub fn open_terminal() -> Result<CrosstermTerminal> {
    enable_raw_mode()?;

    let mut terminal = create_terminal()?;
    //terminal.hide_cursor()?;
    terminal.clear()?;
    Ok(terminal)
}

pub fn close_terminal(terminal: &mut CrosstermTerminal) -> Result<()> {
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    //self.terminal.show_cursor()?;
    disable_raw_mode()?;
    Ok(())
}

fn create_terminal() -> Result<CrosstermTerminal> {
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(stdout))
}
