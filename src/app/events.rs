use std::{
    io,
    panic::{self, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
    thread,
    time::Duration,
};

use ratatui::crossterm::event::{Event, poll, read};

use crate::command::Command;

// Signal handling for graceful shutdown on SIGTERM / SIGINT / SIGHUP.
// Keyboard Ctrl+C never reaches this path: raw mode disables ISIG, so
// only an externally sent SIGINT does.
//
// Without a handler, kill(1) terminates the process instantly, leaving the
// terminal in raw mode with the alternate screen active (broken shell).
//
// Architecture
// ------------
// 1. A POSIX signal handler writes `true` to an `AtomicBool` (the only
//    async-signal-safe operation needed).
// 2. The event-reader thread (spawn_command_sender) polls stdin with a
//    500 ms timeout instead of a blocking read(). After each timeout it
//    checks the flag and sends `Command::Quit` if set.
// 3. A separate watcher thread (spawn_signal_watcher) checks the same flag
//    every 100 ms, independently of the terminal. Step 2 covers a live
//    terminal only: when the terminal dies, `poll` never returns (see
//    `event_loop`) and the reader never reaches another check. That is the
//    case a closing terminal produces, via SIGHUP.
// 4. The main event loop picks up `Command::Quit` from either sender, exits
//    cleanly, and `CleanupOnDropTerminal::Drop` restores the terminal.
//
// The watcher bounds shutdown latency in both cases: its 100 ms interval is
// shorter than the reader's 500 ms poll timeout, so it reaches the flag first
// even while the terminal is alive. The reader's check is the fallback for the
// one case the watcher cannot cover, a watcher thread that failed to spawn.
//
// SA_RESTART
// ----------
// We set SA_RESTART so that the kernel transparently retries interrupted
// syscalls (poll, read, write) after the signal handler returns. This is
// the standard approach: we don't want every syscall to fail with EINTR.
//
// Dropping SA_RESTART would not substitute for the watcher thread. EINTR is
// only raised for a *blocking* syscall, and the wedge in `event_loop` is a
// userspace loop over a fd that is permanently readable at EOF, so no call
// blocks and there is nothing for a signal to interrupt.

static SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);

/// How often `spawn_signal_watcher` checks the signal flag. It bounds shutdown
/// latency when the terminal has died; an atomic load ten times a second costs
/// nothing measurable while it is alive.
const SIGNAL_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Terminal input, abstracted so `event_loop` can be driven by fakes in tests.
trait EventSource {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;

    fn read(&mut self) -> io::Result<Event>;
}

struct TerminalEventSource;

impl EventSource for TerminalEventSource {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        poll(timeout)
    }

    fn read(&mut self) -> io::Result<Event> {
        read()
    }
}

// SAFETY: Stores to an AtomicBool are single-instruction writes to a
// fixed address, async-signal-safe per POSIX.
extern "C" fn handle_signal(_: i32) {
    SIGNAL_RECEIVED.store(true, Ordering::Relaxed);
}

/// Register handlers for termination signals so the app can exit gracefully.
pub fn install_signal_handlers() -> Result<(), nix::errno::Errno> {
    // Safety: sigaction is inherently unsafe but necessary for signal handling.
    // We pass function pointers that only perform atomic stores, which is
    // signal-safe. The handlers are installed once at startup and never removed.
    #[allow(unsafe_code)]
    unsafe {
        use nix::sys::signal::{SigAction, SigHandler, Signal, sigaction};

        let action = SigAction::new(
            SigHandler::Handler(handle_signal),
            // See SA_RESTART note above.
            nix::sys::signal::SaFlags::SA_RESTART,
            nix::sys::signal::SigSet::empty(),
        );
        sigaction(Signal::SIGTERM, &action)?;
        sigaction(Signal::SIGINT, &action)?;
        sigaction(Signal::SIGHUP, &action)?;
    }
    Ok(())
}

pub(super) fn receive_commands(rx: &Receiver<Command>) -> Vec<Command> {
    // Block (zero CPU) until the first command arrives
    let Ok(first) = rx.recv() else {
        // The channel is disconnected: all senders have been dropped. This should
        // not happen in normal operation because App holds tx for its entire lifetime.
        // Returning an empty Vec here would cause App::run to loop forever: it would
        // call receive_commands again immediately (since recv() returns Err instantly
        // on a disconnected channel), burning 100% CPU re-rendering with no commands.
        // Returning Quit exits the loop cleanly instead.
        log::error!("Command channel disconnected unexpectedly");
        return vec![Command::Quit];
    };
    let mut commands = vec![first];
    // Drain everything else already queued, then render once with all of it
    // applied. No cap is needed: the search thread batches its hits
    // (see file_system/search.rs), so no producer floods this channel.
    while let Ok(command) = rx.try_recv() {
        commands.push(command);
    }
    commands
}

/// Sends `Command::Quit` once a termination signal has been seen, independently
/// of the event reader.
///
/// The reader checks the same flag between polls, which is enough while the
/// terminal is alive. It is not enough when the terminal dies: `poll` never
/// returns at EOF (see `event_loop`), so the reader never reaches its next
/// check and the flag it set is never read. Closing a terminal is precisely
/// that case, since the kernel sends SIGHUP to the foreground process group as
/// the pty goes away, so without this thread the process outlives its terminal
/// as an orphan that answers nothing but SIGKILL.
///
/// This does not stop the reader thread from spinning; nothing on this side
/// can. It bounds the spin by how long the process lives, and the process exits
/// within one `SIGNAL_POLL_INTERVAL`, so no orphan is left behind.
pub(super) fn spawn_signal_watcher(tx: Sender<Command>) {
    let builder = thread::Builder::new().name("filectrl-signal-watcher".into());
    let spawn_result = builder.spawn(move || {
        watch_signal_flag(&tx, &SIGNAL_RECEIVED, SIGNAL_POLL_INTERVAL);
    });

    // Not fatal: the reader thread still answers signals for a live terminal,
    // which is every case except the one this thread exists for. Losing the
    // fallback is worth logging, not worth refusing to start over.
    if let Err(err) = spawn_result {
        log::error!("Failed to spawn signal watcher thread: {err}");
    }
}

/// The watcher's loop, over a caller-supplied flag so tests can drive it without
/// touching the process-wide `SIGNAL_RECEIVED` that every test in this binary
/// shares.
fn watch_signal_flag(tx: &Sender<Command>, flag: &AtomicBool, interval: Duration) {
    loop {
        if flag.load(Ordering::Relaxed) {
            // A send error means the receiver is already gone, i.e. the app is
            // shutting down by some other route. Nothing left to do either way.
            let _ = tx.send(Command::Quit);
            return;
        }
        thread::sleep(interval);
    }
}

pub(super) fn spawn_command_sender(tx: Sender<Command>) {
    // 500 ms poll interval: fast enough that shutdown feels instant (<1 s),
    // sparse enough that CPU overhead is negligible (<0.2 % on a single core).
    let poll_interval = Duration::from_millis(500);

    let builder = thread::Builder::new().name("filectrl-event-reader".into());
    let reader_tx = tx.clone();
    let spawn_result = builder.spawn(move || {
        // catch_unwind so a panic in the reader thread is logged instead of
        // silently terminating only the thread (leaving the main loop blocked
        // forever on rx.recv()).
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            event_loop(&reader_tx, poll_interval, &mut TerminalEventSource);
        }));
        if let Err(payload) = result {
            let message = panic_message(payload.as_ref());
            log::error!("Event reader thread panicked: {message}");
            // Wake the main loop so it doesn't block forever.
            let _ = reader_tx.send(Command::Quit);
        }
    });

    if let Err(err) = spawn_result {
        log::error!("Failed to spawn event reader thread: {err}");
        // Without the reader thread there is no terminal input, so the main
        // loop would block on rx.recv() until a signal arrives and the watcher
        // sends Quit. Enqueue Quit now so the app shuts down cleanly instead of
        // sitting at a screen that answers nothing.
        let _ = tx.send(Command::Quit);
    }
}

fn event_loop<S: EventSource>(tx: &Sender<Command>, poll_interval: Duration, source: &mut S) {
    loop {
        // Check the signal flag before each poll so that the window
        // between a signal arriving and us noticing it is bounded by
        // the poll timeout (~500 ms max). Checking first (rather than
        // only after poll returns) also handles the unlikely case where
        // the signal fires between poll() returning Ok(false) and the
        // continue jumping back to the top.
        if SIGNAL_RECEIVED.load(Ordering::Relaxed) {
            // Signal handler fired: ask the main loop to shut down.
            let _ = tx.send(Command::Quit);
            return;
        }

        // poll() with a timeout. Returns Ok(true) if an event is queued
        // (next read() is non-blocking), Ok(false) on timeout.
        //
        // A poll()/read() error means stdin is no longer usable (e.g. the
        // terminal closed or the fd was revoked), so there is nothing to retry:
        // continuing would busy-loop on the same error. We send Command::Quit
        // and exit the reader thread so the main loop wakes from rx.recv(),
        // shuts down, and CleanupOnDropTerminal::Drop restores the terminal.
        // Without this, the main loop would block on rx.recv() forever because
        // this thread is its only command producer.
        //
        // These error arms depend on crossterm reporting a vanished terminal.
        // It currently does not: at EOF the mio event source re-reads a
        // permanently readable fd forever, so poll() never returns and this
        // thread wedges at 100% CPU. Nothing here can detect that, because
        // control never comes back. Revisit when the upstream issue is resolved:
        // https://github.com/crossterm-rs/crossterm/issues/793
        //
        // The process still exits when this happens: the terminal's death
        // delivers SIGHUP and spawn_signal_watcher acts on it from outside this
        // thread. What is lost here is only the ability to quit *first* and
        // skip the spin, not the ability to quit at all.
        let event = match source.poll(poll_interval) {
            Ok(true) => match source.read() {
                Ok(event) => event,
                Err(err) => {
                    log::error!("Failed to read terminal event: {err}");
                    let _ = tx.send(Command::Quit);
                    return;
                }
            },
            Ok(false) => continue,
            Err(err) => {
                log::error!("Failed to poll terminal event: {err}");
                let _ = tx.send(Command::Quit);
                return;
            }
        };

        if let Some(command) = Command::maybe_from(event) {
            // A send error means the receiver (App) has been dropped, i.e.
            // the app is shutting down. Exit the thread cleanly instead of
            // panicking on a late keystroke during teardown.
            if tx.send(command).is_err() {
                return;
            }
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<non-string panic payload>"
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::mpsc, time::Duration};

    use ratatui::crossterm::event::Event;

    use super::{
        AtomicBool, Command, EventSource, Ordering, event_loop, panic_message, watch_signal_flag,
    };

    const INTERVAL: Duration = Duration::from_millis(500);
    // Short enough to keep the watcher tests quick, long enough that the loop
    // sleeps at least once rather than racing through on the first check.
    const WATCH_INTERVAL: Duration = Duration::from_millis(5);

    /// Scripted input. Exhausting the poll script ends `event_loop` through its
    /// production error path, which keeps the tests independent of the global
    /// SIGNAL_RECEIVED flag that other tests in this binary share.
    struct FakeEventSource {
        polls: VecDeque<std::io::Result<bool>>,
        events: VecDeque<Event>,
    }

    impl FakeEventSource {
        fn new(polls: Vec<std::io::Result<bool>>) -> Self {
            Self {
                polls: polls.into(),
                events: VecDeque::new(),
            }
        }

        fn with_events(mut self, events: Vec<Event>) -> Self {
            self.events = events.into();
            self
        }
    }

    impl EventSource for FakeEventSource {
        fn poll(&mut self, _timeout: Duration) -> std::io::Result<bool> {
            self.polls
                .pop_front()
                .unwrap_or_else(|| Err(std::io::Error::other("poll script exhausted")))
        }

        fn read(&mut self) -> std::io::Result<Event> {
            self.events
                .pop_front()
                .ok_or_else(|| std::io::Error::other("event script exhausted"))
        }
    }

    #[test]
    fn timeouts_keep_polling() {
        let (tx, rx) = mpsc::channel();
        let mut source = FakeEventSource::new((0..10).map(|_| Ok(false)).collect());

        event_loop(&tx, INTERVAL, &mut source);

        // The whole script ran, so the only Quit came from its exhaustion.
        assert!(source.polls.is_empty());
        assert_eq!(Some(Command::Quit), rx.try_recv().ok());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_poll_error_shuts_down() {
        let (tx, rx) = mpsc::channel();
        let mut source = FakeEventSource::new(vec![Err(std::io::Error::from(
            std::io::ErrorKind::UnexpectedEof,
        ))]);

        event_loop(&tx, INTERVAL, &mut source);

        assert_eq!(Some(Command::Quit), rx.try_recv().ok());
    }

    #[test]
    fn a_read_error_shuts_down() {
        let (tx, rx) = mpsc::channel();
        let mut source = FakeEventSource::new(vec![Ok(true)]);

        event_loop(&tx, INTERVAL, &mut source);

        assert_eq!(Some(Command::Quit), rx.try_recv().ok());
    }

    #[test]
    fn events_are_forwarded_as_commands() {
        let (tx, rx) = mpsc::channel();
        let mut source =
            FakeEventSource::new(vec![Ok(true)]).with_events(vec![Event::Resize(10, 20)]);

        event_loop(&tx, INTERVAL, &mut source);

        assert_eq!(
            Some(Command::Resize {
                width: 10,
                height: 20
            }),
            rx.try_recv().ok()
        );
    }

    #[test]
    fn a_flag_already_set_quits_without_waiting() {
        let (tx, rx) = mpsc::channel();
        let flag = AtomicBool::new(true);

        watch_signal_flag(&tx, &flag, WATCH_INTERVAL);

        assert_eq!(Some(Command::Quit), rx.try_recv().ok());
    }

    #[test]
    fn a_flag_set_while_waiting_quits() {
        let (tx, rx) = mpsc::channel();
        let flag = AtomicBool::new(false);

        // The watcher must notice a flag raised after it began sleeping, which
        // is the real sequence: the handler runs while this loop is idle.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(WATCH_INTERVAL * 3);
                flag.store(true, Ordering::Relaxed);
            });
            watch_signal_flag(&tx, &flag, WATCH_INTERVAL);
        });

        assert_eq!(Some(Command::Quit), rx.try_recv().ok());
    }

    #[test]
    fn panic_message_extracts_str_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(panic_message(payload.as_ref()), "boom");
    }

    #[test]
    fn panic_message_extracts_string_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(String::from("kaboom"));
        assert_eq!(panic_message(payload.as_ref()), "kaboom");
    }

    #[test]
    fn panic_message_falls_back_for_other_payloads() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(42u32);
        assert_eq!(
            panic_message(payload.as_ref()),
            "<non-string panic payload>"
        );
    }
}
