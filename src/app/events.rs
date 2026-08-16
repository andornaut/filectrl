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
// The watcher is the shutdown route in every case: woken by the byte rather
// than by a timer, it reaches the signal first and costs nothing while none is
// pending. Reading the flag on a timer instead meant waking ten times a second
// for the life of the process and still reacting up to one interval late. The
// reader's check is the fallback for the one case the watcher cannot cover, a
// watcher that never started; its 2 s timeout bounds that fallback alone.
//
// SA_RESTART
// ----------
// Set so the kernel retries interrupted syscalls (poll, read, write) after the
// handler returns, rather than failing them with EINTR.
//
// Dropping it would not substitute for the watcher thread: EINTR is raised only
// for a *blocking* syscall, and the wedge in `event_loop` is a userspace loop
// over an fd that is permanently readable at EOF, so nothing blocks and a
// signal has nothing to interrupt.

static SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Read end of the self-pipe, owned for the life of the process so the watcher
/// can borrow it. Set once by `install_signal_handlers`.
static SIGNAL_PIPE_READ: OnceLock<OwnedFd> = OnceLock::new();

/// Write end of the self-pipe, raw because the signal handler can only reach it
/// through an atomic load. Negative until the pipe exists, which the handler
/// must tolerate: a signal can arrive between `sigaction` and the store.
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
    // Non-blocking, so a pipe filled by a signal storm fails with EAGAIN rather
    // than blocking inside a handler. Nothing is lost: a full pipe already holds
    // a byte the watcher has not read, and one is all it takes to wake it.
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

    // Both ends live as long as the process. The write end is leaked: a closed
    // descriptor in a signal handler would be a use-after-close.
    SIGNAL_PIPE_WRITE_FD.store(write_fd.into_raw_fd(), Ordering::Relaxed);
    let _ = SIGNAL_PIPE_READ.set(read_fd);
    Ok(())
}

/// Register handlers for termination signals so the app can exit gracefully.
pub fn install_signal_handlers() -> Result<(), Errno> {
    // Before `sigaction`, so a signal delivered as soon as the handlers are
    // installed finds a pipe to write to.
    install_signal_pipe()?;

    // Safety: the handler is installed once at startup, never removed, and does
    // only async-signal-safe work. See `handle_signal`.
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
        // Every sender dropped, which App holding tx for its lifetime should
        // make unreachable. An empty Vec would spin App::run at 100% CPU, since
        // recv() returns Err instantly once disconnected; Quit exits cleanly.
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
/// The reader's own check between polls is enough only while the terminal is
/// alive. At EOF `poll` never returns (see `event_loop`), so the reader never
/// reaches its next check and the flag it set is never read. A closing terminal
/// is exactly that case: the kernel sends SIGHUP as the pty goes away, and
/// without this thread the process outlives it as an orphan answering nothing
/// but SIGKILL.
///
/// Nothing on this side can stop the reader spinning. This bounds the spin by
/// how long the process lives, which is until the handler's byte arrives.
pub(super) fn spawn_signal_watcher(tx: Sender<Command>) {
    // Not fatal, here or below: the reader still answers signals for a live
    // terminal, which is every case but the one this thread exists for.
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
/// through a pipe of their own. Blocks until a byte arrives, so a process with
/// no signal pending does no work. The byte's value says nothing; that a handler
/// ran is the whole message.
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
            Err(Errno::EINTR) => (),
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

pub(super) fn spawn_command_sender(tx: &Sender<Command>) {
    // Only the wait for terminal input, so it does not affect how quickly a
    // keystroke is handled. The timeout exists so the signal flag is checked at
    // all, which matters only when no watcher thread is running.
    let poll_interval = Duration::from_secs(2);

    let builder = thread::Builder::new().name("filectrl-event-reader".into());
    let reader_tx = tx.clone();
    let spawn_result = builder.spawn(move || {
        // Without catch_unwind a panic kills only this thread, silently,
        // leaving the main loop blocked on rx.recv() forever.
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
        // No reader means no terminal input, so the main loop would sit on
        // rx.recv() at a screen that answers nothing until a signal arrives.
        let _ = tx.send(Command::Quit);
    }
}

fn event_loop<S: EventSource>(tx: &Sender<Command>, poll_interval: Duration, source: &mut S) {
    loop {
        // Bounds the window between a signal arriving and this thread noticing
        // by the poll timeout. The watcher normally quits first; this answers a
        // signal when it is not running. Checked before the poll rather than
        // after, which also catches a signal that fires between poll() returning
        // Ok(false) and the jump back to the top.
        if SIGNAL_RECEIVED.load(Ordering::Relaxed) {
            let _ = tx.send(Command::Quit);
            return;
        }

        // An error from poll()/read() means stdin is no longer usable (the
        // terminal closed, the fd was revoked), so there is nothing to retry and
        // continuing would busy-loop on it. Quit instead, so the main loop wakes
        // from rx.recv() and CleanupOnDropTerminal::Drop restores the terminal;
        // this thread is its only command producer.
        //
        // Those arms depend on crossterm reporting a vanished terminal, which it
        // does not: at EOF the mio event source re-reads a permanently readable
        // fd forever, so poll() never returns and this thread wedges at 100%
        // CPU. Nothing here can detect that, because control never comes back.
        // Revisit when the upstream issue is resolved:
        // https://github.com/crossterm-rs/crossterm/issues/793
        //
        // The process still exits: SIGHUP reaches spawn_signal_watcher from
        // outside this thread. Only quitting *first*, and skipping the spin, is
        // lost.
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

        if let Some(command) = Command::maybe_from(&event) {
            // A dropped receiver means App is shutting down. Exit cleanly
            // rather than panicking on a late keystroke during teardown.
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
