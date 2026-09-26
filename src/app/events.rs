use std::{
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicI32, Ordering},
        mpsc::{Receiver, Sender},
    },
    thread,
    time::Duration,
};

use nix::{
    errno::Errno,
    poll::PollTimeout,
    sys::signal::{SigSet, Signal, raise},
};
use ratatui::crossterm::event::{Event, poll, read};

use crate::command::Command;

// The signals below are blocked in every thread and taken by one thread with
// `sigwait`, so nothing runs in a signal handler. Raw mode disables ISIG, so
// while the interface is up only externally sent signals arrive.
// A child inherits the mask, so every spawn goes through `unblock_in_child`.

/// The signals `block_signals` blocks and the signal thread answers.
fn handled_signals() -> SigSet {
    let mut signals = SigSet::empty();
    for signal in [
        Signal::SIGTERM,
        Signal::SIGINT,
        Signal::SIGHUP,
        Signal::SIGQUIT,
        Signal::SIGTSTP,
        Signal::SIGUSR1,
        Signal::SIGUSR2,
        Signal::SIGALRM,
    ] {
        signals.add(signal);
    }
    signals
}

/// Blocks the handled signals in the calling thread and every thread it
/// spawns afterwards. Call before any thread is spawned.
pub fn block_signals() -> nix::Result<()> {
    handled_signals().thread_block()
}

/// Unblocks the handled signals in the child `command` starts, which would
/// otherwise inherit the mask and never receive them.
pub(crate) fn unblock_in_child(command: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::unix::process::CommandExt;

    let signals = handled_signals();
    // SAFETY: `pthread_sigmask` is async-signal-safe and `signals` is built
    // before the fork, so the closure neither allocates nor locks.
    #[allow(unsafe_code)]
    unsafe {
        command.pre_exec(move || signals.thread_unblock().map_err(io::Error::from))
    }
}

/// Set while a program runs in the terminal's foreground.
static FOREGROUND_CHILD: AtomicBool = AtomicBool::new(false);

/// Marks the start or end of a program running in the terminal's foreground.
pub(super) fn set_foreground_child(running: bool) {
    FOREGROUND_CHILD.store(running, Ordering::SeqCst);
}

/// The first quit signal to arrive, 0 before any has.
static QUIT_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// The signal that ended the run, if one did. A terminal hangup counts as
/// SIGHUP.
pub fn quit_signal() -> Option<i32> {
    match QUIT_SIGNAL.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

/// Records `signal` as the cause of the quit, keeping the first, and asks the
/// app to quit.
fn quit(tx: &Sender<Command>, signal: Signal) {
    let _ = QUIT_SIGNAL.compare_exchange(0, signal as i32, Ordering::SeqCst, Ordering::SeqCst);
    let _ = tx.send(Command::Quit);
}

#[derive(Debug, PartialEq)]
enum Answer {
    Ignore,
    /// Stop this process, so the shell regains the terminal and `fg` resumes
    /// it together with the program, which shares its process group.
    Stop,
    Quit,
}

/// While a program runs, Ctrl+C and Ctrl+\ are its own (as with `system(3)`)
/// and Ctrl+Z stops both. Otherwise SIGTSTP is ignored, since a stopped process
/// would leave the terminal in raw mode.
fn answer(child_running: bool, signal: Signal) -> Answer {
    match signal {
        Signal::SIGTSTP if child_running => Answer::Stop,
        Signal::SIGTSTP => Answer::Ignore,
        Signal::SIGINT | Signal::SIGQUIT if child_running => Answer::Ignore,
        _ => Answer::Quit,
    }
}

/// Answers the handled signals until the process exits. They are blocked, so
/// one that arrived before this thread started is answered once it does.
pub(super) fn spawn_signal_thread(tx: Sender<Command>) -> io::Result<()> {
    let signals = handled_signals();
    thread::Builder::new()
        .name("filectrl-signals".into())
        .spawn(move || {
            loop {
                let signal = match signals.wait() {
                    Ok(signal) => signal,
                    Err(err) => {
                        log::error!("Failed to wait for a signal: {err}");
                        return;
                    }
                };
                match answer(FOREGROUND_CHILD.load(Ordering::SeqCst), signal) {
                    Answer::Ignore => (),
                    // SIGSTOP rather than SIGTSTP, which is blocked here.
                    Answer::Stop => {
                        let _ = raise(Signal::SIGSTOP);
                    }
                    Answer::Quit => {
                        log::info!("Quitting on {signal}");
                        quit(&tx, signal);
                    }
                }
            }
        })
        .map(drop)
}

/// Sends `Command::Quit` when the terminal hangs up, recorded as SIGHUP.
///
/// WORKAROUND: at EOF crossterm re-reads a permanently readable fd forever, so
/// the reader's `poll` never returns and that thread spins; this thread still
/// quits the process. It is needed even with the signal thread, since the
/// kernel sends SIGHUP only to the session leader, which may ignore it
/// (`trap "" HUP`). https://github.com/crossterm-rs/crossterm/issues/793
pub(super) fn spawn_hangup_watcher(tx: Sender<Command>) {
    let Some(terminal) = reader_terminal() else {
        log::debug!("Cannot watch for a hangup: no terminal to poll");
        return;
    };
    let spawned = thread::Builder::new()
        .name("filectrl-hangup".into())
        .spawn(move || {
            if wait_for_hangup(PollTimeout::NONE, terminal.as_fd()) {
                log::info!("The terminal hung up");
                quit(&tx, Signal::SIGHUP);
            }
        });
    if let Err(err) = spawned {
        log::error!("Failed to spawn the hangup watcher thread: {err}");
    }
}

/// The terminal crossterm reads: stdin if it is a terminal, else /dev/tty.
fn reader_terminal() -> Option<OwnedFd> {
    use std::io::IsTerminal;

    let stdin = io::stdin();
    if stdin.is_terminal() {
        return stdin.as_fd().try_clone_to_owned().ok();
    }
    std::fs::File::open("/dev/tty").ok().map(OwnedFd::from)
}

/// Waits up to `timeout` for `terminal` to hang up, returning whether it did.
/// A terminal that cannot be polled (macOS answers `POLLNVAL` for a terminal
/// device) returns false at once.
fn wait_for_hangup(timeout: PollTimeout, terminal: BorrowedFd<'_>) -> bool {
    use nix::poll::{PollFd, PollFlags, poll};

    loop {
        let mut fds = [PollFd::new(terminal, PollFlags::empty())];
        match poll(&mut fds, timeout) {
            Ok(0) => return false,
            Ok(_) => (),
            // SIGWINCH is not blocked, so it can land on this thread.
            Err(Errno::EINTR) => continue,
            Err(err) => {
                log::error!("Failed to poll the terminal for a hangup: {err}");
                return false;
            }
        }
        let events = fds[0].revents().unwrap_or(PollFlags::empty());
        if events.contains(PollFlags::POLLNVAL) {
            log::debug!("The terminal cannot be polled for a hangup");
            return false;
        }
        if events.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
            return true;
        }
    }
}

/// Terminal input, abstracted for tests.
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

pub(super) fn receive_commands(rx: &Receiver<Command>) -> Vec<Command> {
    let Ok(first) = rx.recv() else {
        // Unreachable while App holds `tx`. An empty Vec would spin App::run, since
        // `recv` fails at once when disconnected.
        log::error!("Command channel disconnected unexpectedly");
        return vec![Command::Quit];
    };
    let mut commands = vec![first];
    // Drain what is queued so the batch renders once.
    while let Ok(command) = rx.try_recv() {
        commands.push(command);
    }
    commands
}

/// Pauses the reader thread so a foreground program reads the terminal alone.
#[derive(Default)]
pub(super) struct ReaderGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum GateState {
    #[default]
    Open,
    PauseRequested,
    Paused,
}

impl ReaderGate {
    /// Stops the reader at its next checkpoint and waits until it has.
    pub(super) fn pause(&self) {
        let mut state = self.lock();
        *state = GateState::PauseRequested;
        // Makes crossterm's poll return, so the reader reaches its checkpoint now
        // rather than at the poll timeout.
        let _ = raise(Signal::SIGWINCH);
        let _state = self
            .changed
            .wait_while(state, |state| *state != GateState::Paused)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }

    pub(super) fn resume(&self) {
        *self.lock() = GateState::Open;
        self.changed.notify_all();
    }

    /// The reader's side: blocks from a requested pause until `resume`.
    pub(super) fn checkpoint(&self) {
        let mut state = self.lock();
        if *state != GateState::PauseRequested {
            return;
        }
        *state = GateState::Paused;
        self.changed.notify_all();
        let _state = self
            .changed
            .wait_while(state, |state| *state == GateState::Paused)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(super) fn spawn_command_sender(tx: &Sender<Command>, gate: Arc<ReaderGate>) {
    // Only bounds a pause whose wake-up was lost.
    let poll_interval = Duration::from_secs(2);

    let builder = thread::Builder::new().name("filectrl-event-reader".into());
    let reader_tx = tx.clone();
    let spawn_result = builder.spawn(move || {
        event_loop(&reader_tx, &gate, poll_interval, &mut TerminalEventSource);
    });

    if let Err(err) = spawn_result {
        log::error!("Failed to spawn event reader thread: {err}");
        // No reader means no input: quit rather than hang.
        let _ = tx.send(Command::Quit);
    }
}

fn event_loop<S: EventSource>(
    tx: &Sender<Command>,
    gate: &ReaderGate,
    poll_interval: Duration,
    source: &mut S,
) {
    loop {
        gate.checkpoint();

        // A poll or read error means stdin is unusable, so quit. At EOF the poll
        // never returns instead; see `spawn_hangup_watcher`.
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
            // The receiver is gone: the app is shutting down.
            if tx.send(command).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, os::fd::AsFd, sync::mpsc, time::Duration};

    use nix::{poll::PollTimeout, sys::signal::Signal};
    use ratatui::crossterm::event::Event;
    use test_case::test_case;

    use super::{
        Answer, Command, EventSource, ReaderGate, answer, event_loop, receive_commands,
        wait_for_hangup,
    };

    const INTERVAL: Duration = Duration::from_millis(500);

    /// Scripted input. An exhausted poll script ends `event_loop` through its error
    /// path.
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
    fn a_poll_that_times_out_keeps_the_loop_running() {
        let (tx, rx) = mpsc::channel();
        let mut source = FakeEventSource::new((0..10).map(|_| Ok(false)).collect());

        event_loop(&tx, &ReaderGate::default(), INTERVAL, &mut source);

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

        event_loop(&tx, &ReaderGate::default(), INTERVAL, &mut source);

        assert_eq!(Some(Command::Quit), rx.try_recv().ok());
    }

    #[test]
    fn a_read_error_shuts_down() {
        let (tx, rx) = mpsc::channel();
        let mut source = FakeEventSource::new(vec![Ok(true)]);

        event_loop(&tx, &ReaderGate::default(), INTERVAL, &mut source);

        assert_eq!(Some(Command::Quit), rx.try_recv().ok());
    }

    #[test]
    fn events_are_forwarded_as_commands() {
        let (tx, rx) = mpsc::channel();
        let mut source =
            FakeEventSource::new(vec![Ok(true)]).with_events(vec![Event::Resize(10, 20)]);

        event_loop(&tx, &ReaderGate::default(), INTERVAL, &mut source);

        assert_eq!(
            Some(Command::Resize {
                width: 10,
                height: 20
            }),
            rx.try_recv().ok()
        );
    }

    #[test]
    fn a_closed_channel_stops_the_reader() {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let mut source = FakeEventSource::new(vec![Ok(true), Ok(true)])
            .with_events(vec![Event::Resize(10, 20), Event::Resize(30, 40)]);

        event_loop(&tx, &ReaderGate::default(), INTERVAL, &mut source);

        assert_eq!(1, source.events.len());
    }

    #[test]
    fn everything_already_queued_is_received_as_one_batch() {
        let (tx, rx) = mpsc::channel();
        tx.send(Command::SearchTick).unwrap();
        tx.send(Command::ResetView).unwrap();

        assert_eq!(
            vec![Command::SearchTick, Command::ResetView],
            receive_commands(&rx)
        );
    }

    #[test]
    fn a_disconnected_channel_is_received_as_quit() {
        let (tx, rx) = mpsc::channel();
        drop(tx);

        assert_eq!(vec![Command::Quit], receive_commands(&rx));
    }

    fn pty() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        let pty = nix::pty::openpty(None, None).expect("a pty should be creatable");
        (pty.master, pty.slave)
    }

    // macOS cannot poll a terminal device.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_terminal_that_hangs_up_is_a_hangup() {
        let (master, slave) = pty();
        drop(master);

        assert!(wait_for_hangup(PollTimeout::from(1000u16), slave.as_fd()));
    }

    #[test]
    fn a_live_terminal_with_input_is_not_a_hangup() {
        let (master, slave) = pty();
        nix::unistd::write(&master, b"q\n").expect("the pty should accept input");

        assert!(!wait_for_hangup(PollTimeout::from(50u16), slave.as_fd()));
    }

    // macOS blocks in `poll` on an fd number past its table instead of
    // reporting POLLNVAL; the timeout keeps a regression from hanging.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_terminal_that_cannot_be_polled_is_not_a_hangup() {
        // SAFETY: a closed fd number, only handed to `poll`.
        #[allow(unsafe_code)]
        let closed = unsafe { std::os::fd::BorrowedFd::borrow_raw(1 << 20) };

        assert!(!wait_for_hangup(PollTimeout::from(5000u16), closed));
    }

    /// A signal left unblocked kills the process with the terminal still raw.
    #[test]
    fn every_terminating_signal_is_answered() {
        let handled = super::handled_signals();
        for signal in [
            Signal::SIGTERM,
            Signal::SIGINT,
            Signal::SIGHUP,
            Signal::SIGQUIT,
            Signal::SIGTSTP,
            Signal::SIGUSR1,
            Signal::SIGUSR2,
            Signal::SIGALRM,
        ] {
            assert!(handled.contains(signal), "{signal:?}");
        }
    }

    #[test_case(false, Signal::SIGTERM => Answer::Quit ; "sigterm quits")]
    #[test_case(false, Signal::SIGHUP => Answer::Quit ; "sighup quits")]
    #[test_case(false, Signal::SIGINT => Answer::Quit ; "sigint quits")]
    #[test_case(false, Signal::SIGQUIT => Answer::Quit ; "sigquit quits")]
    #[test_case(false, Signal::SIGTSTP => Answer::Ignore ; "sigtstp is ignored")]
    #[test_case(true, Signal::SIGTERM => Answer::Quit ; "sigterm quits during a program")]
    #[test_case(true, Signal::SIGHUP => Answer::Quit ; "sighup quits during a program")]
    #[test_case(true, Signal::SIGINT => Answer::Ignore ; "sigint is the programs")]
    #[test_case(true, Signal::SIGQUIT => Answer::Ignore ; "sigquit is the programs")]
    #[test_case(true, Signal::SIGTSTP => Answer::Stop ; "sigtstp stops with the program")]
    fn a_signal_is_answered_by(child_running: bool, signal: Signal) -> Answer {
        answer(child_running, signal)
    }

    /// Blocks the handled signals on this test's thread only, as `block_signals`
    /// does for the whole process. `sleep` rather than a shell, which may clear
    /// its own mask.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_receives_the_signals_this_process_blocks() {
        super::handled_signals().thread_block().unwrap();
        let spawned =
            super::unblock_in_child(std::process::Command::new("sleep").arg("10")).spawn();
        super::handled_signals().thread_unblock().unwrap();
        let mut child = spawned.unwrap();
        let status = std::fs::read_to_string(format!("/proc/{}/status", child.id()));
        let _ = child.kill();
        let _ = child.wait();

        let status = status.unwrap();
        assert!(status.contains("SigBlk:\t0000000000000000\n"), "{status}");
    }

    #[test]
    fn a_paused_reader_stops_at_its_checkpoint_until_resumed() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let gate = Arc::new(ReaderGate::default());
        let passes = Arc::new(AtomicUsize::new(0));
        let reader = {
            let (gate, passes) = (gate.clone(), passes.clone());
            std::thread::spawn(move || {
                // Counts between sleeping and the checkpoint, so a pause that
                // returns while the reader sleeps sees the count move.
                while passes.load(Ordering::SeqCst) < 1_000_000 {
                    std::thread::sleep(Duration::from_millis(10));
                    passes.fetch_add(1, Ordering::SeqCst);
                    gate.checkpoint();
                }
            })
        };
        // Into the reader's second sleep.
        while passes.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(2));

        gate.pause();
        let stopped_at = passes.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(stopped_at, passes.load(Ordering::SeqCst));

        gate.resume();
        passes.store(1_000_000, Ordering::SeqCst);
        reader.join().unwrap();
    }
}
