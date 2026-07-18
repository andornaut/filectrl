use std::{
    env,
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
            DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
            PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        },
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

/// Process-wide "already restored" guard shared by every cleanup path:
/// whichever runs first performs the restore, the rest become no-ops.
/// A second `PopKeyboardEnhancementFlags` after leaving the alternate screen
/// would pop an entry from the main screen's stack that this program never
/// pushed. A static is required because the panic hook is a `'static`
/// closure that cannot reference instance state; `try_new` re-arms it.
static TERMINAL_RESTORED: AtomicBool = AtomicBool::new(false);

/// Restores the terminal at most once per acquisition (see
/// `TERMINAL_RESTORED`). Callable from any cleanup path.
fn restore_terminal_once() {
    if !TERMINAL_RESTORED.swap(true, Ordering::SeqCst) {
        restore_terminal();
    }
}

/// Undoes everything `try_new` set up, in one shared sequence so the cleanup
/// paths cannot drift. Errors are ignored: this runs in exit and panic paths
/// where there is nothing useful to do with them.
fn restore_terminal() {
    let _ = execute!(
        stdout(),
        Show,
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        LeaveAlternateScreen,
    );
    let _ = disable_raw_mode();
}

/// A terminal wrapper that restores the terminal state on drop.
///
/// # Cleanup strategy
///
/// Two cleanup paths exist, each covering scenarios the other cannot:
///
/// 1. **`Drop`**: runs on normal exit and on panics in debug builds (which
///    unwind the stack). This is the primary path for the happy path and for
///    development-time crashes.
///
/// 2. **Panic hook** (installed in `try_new`): runs on panics in *release*
///    builds, where `panic = "abort"` skips unwinding entirely and therefore
///    never calls `Drop`. Without the hook, a release-build panic would leave
///    the shell in raw mode / alternate screen.
///
/// In debug builds a panic triggers *both* paths (hook fires, then `Drop`
/// runs as the stack unwinds); `TERMINAL_RESTORED` makes whichever runs
/// first the only one to emit escape sequences.
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
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            )?;

            let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
            terminal.hide_cursor()?;
            terminal.clear()?;
            Ok(Self { terminal })
        };
        build().inspect_err(|_| restore_terminal_once())
    }

    fn cleanup(&mut self) {
        restore_terminal_once();
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
