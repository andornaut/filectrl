use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
    thread,
    time::Duration,
};

use ratatui::crossterm::event::{poll, read};

use crate::command::Command;

// Signal handling for graceful shutdown on SIGTERM / SIGHUP.
//
// Previously, kill(1) would terminate the process instantly, leaving the
// terminal in raw mode and the alternate screen active (broken shell).
//
// Architecture
// ------------
// 1. A POSIX signal handler writes `true` to an `AtomicBool` (the only
//    async-signal-safe operation needed).
// 2. The event-reader thread (spawn_command_sender) polls stdin with a
//    500 ms timeout instead of a blocking read(). After each timeout it
//    checks the flag and sends `Command::Quit` if set.
// 3. The main event loop picks up `Command::Quit`, exits cleanly, and
//    `CleanupOnDropTerminal::Drop` restores the terminal.
//
// SA_RESTART
// ----------
// We set SA_RESTART so that the kernel transparently retries interrupted
// syscalls (poll, read, write) after the signal handler returns. This is
// the standard approach: we don't want every syscall to fail with EINTR
// — the signal flag is already set, and the next poll timeout will detect
// it.  SA_RESTART keeps the rest of the I/O path simple.

static SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);

// SAFETY: Stores to an AtomicBool are single-instruction writes to a
// fixed address — async-signal-safe per POSIX.
extern "C" fn handle_signal(_: i32) {
    SIGNAL_RECEIVED.store(true, Ordering::Relaxed);
}

/// Register handlers for termination signals so the app can exit gracefully.
pub fn install_signal_handlers() -> Result<(), nix::errno::Errno> {
    // Safety: sigaction is inherently unsafe but necessary for signal handling.
    // We pass function pointers that only perform atomic stores, which is
    // signal-safe. The handlers are installed once at startup and never removed.
    unsafe {
        use nix::sys::signal::{SigAction, SigHandler, Signal, sigaction};

        let action = SigAction::new(
            SigHandler::Handler(handle_signal),
            // See SA_RESTART note above.
            nix::sys::signal::SaFlags::SA_RESTART,
            nix::sys::signal::SigSet::empty(),
        );
        sigaction(Signal::SIGTERM, &action)?;
        sigaction(Signal::SIGHUP, &action)?;
    }
    Ok(())
}

pub(super) fn receive_commands(rx: &Receiver<Command>) -> Vec<Command> {
    // Block (zero CPU) until the first command arrives
    let Ok(first) = rx.recv() else {
        // The channel is disconnected — all senders have been dropped. This should
        // not happen in normal operation because App holds tx for its entire lifetime.
        // Returning an empty Vec here would cause App::run to loop forever: it would
        // call receive_commands again immediately (since recv() returns Err instantly
        // on a disconnected channel), burning 100% CPU re-rendering with no commands.
        // Returning Quit exits the loop cleanly instead.
        log::error!("Command channel disconnected unexpectedly");
        return vec![Command::Quit];
    };
    let mut commands = vec![first];
    // Drain any additional commands already queued
    while let Ok(command) = rx.try_recv() {
        commands.push(command);
    }
    commands
}

pub(super) fn spawn_command_sender(tx: Sender<Command>) {
    // 500 ms poll interval: fast enough that shutdown feels instant (<1 s),
    // sparse enough that CPU overhead is negligible (<0.2 % on a single core).
    let poll_interval = Duration::from_millis(500);

    thread::spawn(move || {
        loop {
            // Check the signal flag before each poll so that the window
            // between a signal arriving and us noticing it is bounded by
            // the poll timeout (~500 ms max). Checking first (rather than
            // only after poll returns) also handles the unlikely case where
            // the signal fires between poll() returning Ok(false) and the
            // continue jumping back to the top.
            if SIGNAL_RECEIVED.load(Ordering::Relaxed) {
                // Signal handler fired — ask the main loop to shut down.
                let _ = tx.send(Command::Quit);
                break;
            }

            // poll() with a timeout. Returns Ok(true) if an event is queued
            // (next read() is non-blocking), Ok(false) on timeout.
            let event = match poll(poll_interval) {
                Ok(true) => match read() {
                    Ok(event) => event,
                    Err(err) => {
                        log::error!("Failed to read terminal event: {err}");
                        break;
                    }
                },
                Ok(false) => continue,
                Err(err) => {
                    log::error!("Failed to poll terminal event: {err}");
                    break;
                }
            };

            if let Some(command) = Command::maybe_from(event) {
                // A send error means the receiver (App) has been dropped, i.e.
                // the app is shutting down. Exit the thread cleanly instead of
                // panicking on a late keystroke during teardown.
                if tx.send(command).is_err() {
                    break;
                }
            }
        }
    });
}
