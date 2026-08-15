use std::{
    io,
    os::fd::{AsFd, BorrowedFd, IntoRawFd, OwnedFd},
    panic::{self, AssertUnwindSafe},
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicI32, Ordering},
        mpsc::{Receiver, Sender},
    },
    thread,
    time::Duration,
};

use nix::errno::Errno;
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
// 1. A POSIX signal handler sets an `AtomicBool` and writes one byte to a
//    self-pipe. Both are async-signal-safe: a single-instruction atomic store
//    and `write(2)`.
// 2. A watcher thread (spawn_signal_watcher) blocks in `read()` on the other
//    end of that pipe, independently of the terminal, and sends
//    `Command::Quit` when the byte arrives.
// 3. The event-reader thread (spawn_command_sender) polls stdin with a 2 s
//    timeout instead of a blocking read(). After each timeout it checks the
//    flag and sends `Command::Quit` if set.
// 4. The main event loop picks up `Command::Quit` from either sender, exits
//    cleanly, and `CleanupOnDropTerminal::Drop` restores the terminal.
//
// The watcher is the shutdown route in every case: it is woken by the byte
// rather than by a timer, so it reaches the signal first and costs nothing
// while none is pending. The reader's check is the fallback for the one case
// the watcher cannot cover, a watcher that never started because the pipe or
// the thread could not be created; its 2 s timeout bounds that fallback alone.
//
// The pipe is what makes the watcher free. Reading the flag on a timer meant
// waking ten times a second for the entire life of the process to answer a
// question that is almost always "no", and still reacting up to one interval
// late; a blocking read waits at no cost and returns as soon as the handler
// writes.
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

/// Read end of the self-pipe, owned for the life of the process so the watcher
/// can borrow it. Set once by `install_signal_handlers`.
static SIGNAL_PIPE_READ: OnceLock<OwnedFd> = OnceLock::new();

/// Write end of the self-pipe, as a raw descriptor because the signal handler
/// can only reach it through an atomic load. Negative until the pipe exists,
/// which is the state the handler must tolerate: a signal can be delivered
/// between `sigaction` and the store.
static SIGNAL_PIPE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

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
// fixed address, async-signal-safe per POSIX, as is `write(2)`.
extern "C" fn handle_signal(_: i32) {
    SIGNAL_RECEIVED.store(true, Ordering::Relaxed);

    let fd = SIGNAL_PIPE_WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        // No pipe: only the flag is available, which the reader thread checks.
        return;
    }
    // The write end is non-blocking, so a pipe filled by a signal storm fails
    // with EAGAIN rather than blocking inside a handler. Nothing is lost by
    // that: a full pipe already holds bytes the watcher has not read, and one
    // is all it takes to wake it.
    #[allow(unsafe_code)]
    unsafe {
        let byte: u8 = 0;
        let _ = nix::libc::write(fd, std::ptr::addr_of!(byte).cast(), 1);
    }
}

/// Creates the self-pipe the signal handler writes to, publishing both ends for
/// `handle_signal` and `spawn_signal_watcher`.
fn install_signal_pipe() -> Result<(), Errno> {
    use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};

    let (read_fd, write_fd) = nix::unistd::pipe()?;
    // Close both ends on exec, so a program launched from the file manager
    // inherits neither. Set here rather than at creation because `pipe2`,
    // which takes the flag directly, does not exist on macOS.
    fcntl(&read_fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    fcntl(&write_fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    // The write end alone is non-blocking: a handler must never wait, and the
    // read end staying blocking is what lets the watcher wait for free.
    fcntl(&write_fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;

    // Both ends live as long as the process: the write end is leaked because a
    // closed descriptor in a signal handler would be a use-after-close, and the
    // read end is owned by the static the watcher borrows from.
    SIGNAL_PIPE_WRITE_FD.store(write_fd.into_raw_fd(), Ordering::Relaxed);
    let _ = SIGNAL_PIPE_READ.set(read_fd);
    Ok(())
}

/// Register handlers for termination signals so the app can exit gracefully.
pub fn install_signal_handlers() -> Result<(), Errno> {
    // Before `sigaction`, so a signal delivered as soon as the handlers are
    // installed finds a pipe to write to.
    install_signal_pipe()?;

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
/// The reader checks the signal flag between polls, which is enough while the
/// terminal is alive. It is not enough when the terminal dies: `poll` never
/// returns at EOF (see `event_loop`), so the reader never reaches its next
/// check and the flag it set is never read. Closing a terminal is precisely
/// that case, since the kernel sends SIGHUP to the foreground process group as
/// the pty goes away, so without this thread the process outlives its terminal
/// as an orphan that answers nothing but SIGKILL.
///
/// This does not stop the reader thread from spinning; nothing on this side
/// can. It bounds the spin by how long the process lives, and the process exits
/// as soon as the handler's byte arrives, so no orphan is left behind.
pub(super) fn spawn_signal_watcher(tx: Sender<Command>) {
    // Not fatal, here or below: the reader thread still answers signals for a
    // live terminal, which is every case except the one this thread exists for.
    // Losing the fallback is worth logging, not worth refusing to start over.
    let Some(read_fd) = SIGNAL_PIPE_READ.get() else {
        log::error!("Cannot watch for signals: the self-pipe was not created");
        return;
    };
    let read_fd = read_fd.as_fd();

    let builder = thread::Builder::new().name("filectrl-signal-watcher".into());
    let spawn_result = builder.spawn(move || {
        watch_signal_pipe(&tx, read_fd);
    });

    if let Err(err) = spawn_result {
        log::error!("Failed to spawn signal watcher thread: {err}");
    }
}

/// The watcher's body, over a caller-supplied descriptor so tests can drive it
/// through a pipe of their own rather than the process-wide one.
///
/// Blocks until a byte arrives, so a process with no signal pending performs no
/// work at all. What the byte says does not matter, only that a handler ran.
fn watch_signal_pipe(tx: &Sender<Command>, read_fd: BorrowedFd<'_>) {
    let mut buffer = [0u8; 1];
    loop {
        match nix::unistd::read(read_fd, &mut buffer) {
            Ok(0) => {
                // Every write end closed. Unreachable while the process lives,
                // since the handler's end is leaked open deliberately.
                log::error!("The signal self-pipe reached end of file");
                return;
            }
            Ok(_) => break,
            // SA_RESTART covers our own handlers, so this is only reachable for
            // a signal installed elsewhere. Resume the wait rather than
            // treating it as a shutdown.
            Err(Errno::EINTR) => continue,
            Err(err) => {
                log::error!("Failed to read the signal self-pipe: {err}");
                return;
            }
        }
    }
    // A send error means the receiver is already gone, i.e. the app is
    // shutting down by some other route. Nothing left to do either way.
    let _ = tx.send(Command::Quit);
}

pub(super) fn spawn_command_sender(tx: Sender<Command>) {
    // Only the wait for terminal input, so its length has no effect on how
    // quickly a keystroke is handled. The timeout exists so the signal flag is
    // checked at all, and that check matters only when no watcher thread is
    // running, which leaves nothing to justify waking twice a second.
    let poll_interval = Duration::from_secs(2);

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
        // Check the signal flag before each poll so that the window between a
        // signal arriving and us noticing it is bounded by the poll timeout.
        // The watcher thread normally quits first; this is what answers a
        // signal when it is not running. Checking first (rather than only after
        // poll returns) also handles the unlikely case where the signal fires
        // between poll() returning Ok(false) and the continue jumping back to
        // the top.
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

    use std::os::fd::AsFd;

    use super::{Command, EventSource, event_loop, panic_message, watch_signal_pipe};

    const INTERVAL: Duration = Duration::from_millis(500);
    // Long enough that the watcher is blocked in `read` before the byte is
    // written, short enough not to slow the suite down.
    const WRITE_DELAY: Duration = Duration::from_millis(5);

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
    fn a_byte_already_written_quits_without_waiting() {
        let (tx, rx) = mpsc::channel();
        let (read_fd, write_fd) = nix::unistd::pipe().expect("a pipe should be creatable");
        nix::unistd::write(&write_fd, &[0]).expect("the pipe should accept a byte");

        watch_signal_pipe(&tx, read_fd.as_fd());

        assert_eq!(Some(Command::Quit), rx.try_recv().ok());
    }

    #[test]
    fn a_byte_written_while_waiting_quits() {
        let (tx, rx) = mpsc::channel();
        let (read_fd, write_fd) = nix::unistd::pipe().expect("a pipe should be creatable");

        // The watcher must wake on a byte written after it began blocking,
        // which is the real sequence: the handler runs while it is idle.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(WRITE_DELAY);
                nix::unistd::write(&write_fd, &[0]).expect("the pipe should accept a byte");
            });
            watch_signal_pipe(&tx, read_fd.as_fd());
        });

        assert_eq!(Some(Command::Quit), rx.try_recv().ok());
    }

    #[test]
    fn a_closed_pipe_does_not_quit() {
        let (tx, rx) = mpsc::channel();
        let (read_fd, write_fd) = nix::unistd::pipe().expect("a pipe should be creatable");
        // End of file rather than a signal, so nothing was asked for.
        drop(write_fd);

        watch_signal_pipe(&tx, read_fd.as_fd());

        assert!(rx.try_recv().is_err());
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
