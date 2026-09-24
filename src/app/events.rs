use std::{
    io,
    os::fd::{AsFd, BorrowedFd, IntoRawFd, OwnedFd},
    process::ExitStatus,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicI32, Ordering},
        mpsc::{Receiver, Sender},
    },
    thread,
    time::Duration,
};

use nix::errno::Errno;
use ratatui::crossterm::event::{Event, poll, read};

use crate::command::Command;

// Signal handling for graceful shutdown on SIGTERM / SIGINT / SIGHUP /
// SIGQUIT / SIGUSR1 / SIGUSR2 / SIGALRM, the terminating signals a user or
// another program is likely to send. Keyboard Ctrl+C, Ctrl+\ and Ctrl+Z never
// reach this path: raw mode disables ISIG, so only an externally sent signal
// does.
//
// Without a handler, kill(1) terminates the process instantly, leaving the
// terminal in raw mode with the alternate screen active (broken shell).
//
// SIGTSTP stops the process by default, which leaves the terminal just as
// broken for as long as it stays stopped. It gets a handler that does nothing
// rather than SIG_IGN: an ignored disposition is inherited across exec, so a
// program launched from the file manager could not be suspended either, while
// a caught one is reset to the default.
//
// Architecture
// ------------
// 1. A POSIX signal handler records the signal in an atomic and writes one
//    byte to a self-pipe. Both are async-signal-safe: an atomic
//    compare-and-swap and `write(2)`.
// 2. A watcher thread (spawn_signal_watcher) blocks in `poll()` on the other
//    end of that pipe, independently of the event reader, and sends
//    `Command::Quit` when the byte arrives. It also watches the terminal for a
//    hangup, which is answered as SIGHUP is (see `quit_on_hangup`).
// 3. The event-reader thread (spawn_command_sender) polls stdin with a 2 s
//    timeout instead of a blocking read(). After each timeout it checks for a
//    recorded quit and sends `Command::Quit` if there is one.
// 4. The main event loop picks up `Command::Quit` from either sender, exits
//    cleanly, and `CleanupOnDropTerminal::Drop` restores the terminal.
//
// The watcher is the shutdown route in every case: woken by the byte rather
// than by a timer, it reaches the signal first and costs nothing while none is
// pending. Reading the recorded quit on a timer instead meant waking ten times a second
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

/// Set while a program runs in the foreground of the terminal (an editor or a
/// pager). Ctrl+C and Ctrl+\ then reach the whole foreground process group,
/// this process included, and are the child's to answer, so the handler lets
/// them pass, as `system(3)` ignores them in the caller.
static FOREGROUND_CHILD: AtomicBool = AtomicBool::new(false);

/// Held by every test that changes a signal's action, so none reads one
/// another has changed for the moment.
#[cfg(test)]
pub(super) static SIGNAL_ACTIONS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(super) fn is_foreground_child() -> bool {
    FOREGROUND_CHILD.load(Ordering::SeqCst)
}

/// Marks the start or end of a program running in the terminal's foreground.
pub(super) fn set_foreground_child(running: bool) {
    FOREGROUND_CHILD.store(running, Ordering::SeqCst);
}

/// Whether a termination signal has arrived, or the terminal hung up.
pub(super) fn quit_requested() -> bool {
    QUIT_SIGNAL.load(Ordering::SeqCst) != 0
}

/// The first termination signal to arrive, 0 before any has.
static QUIT_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// Keeps `signal` in `slot` unless one is there already, so the exit status
/// names the signal that caused the quit rather than one arriving during it.
fn record_first(slot: &AtomicI32, signal: i32) {
    let _ = slot.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
}

/// The termination signal that ended the run, if one did.
pub fn quit_signal() -> Option<i32> {
    match QUIT_SIGNAL.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

/// The foreground program's process id while one runs, else 0. The handler
/// passes a quit signal on to it, so the wait for it returns and the quit is
/// handled rather than queued behind a program that never exits. Cleared
/// before the program is reaped, so a pid the kernel has handed to another
/// process is never signalled.
static FOREGROUND_PID: AtomicI32 = AtomicI32::new(0);

/// Runs `command` as the program in the terminal's foreground and waits for it
/// to exit. `quit_requested` is checked before it starts and again once its
/// pid is known, so a quit that arrives first is not left waiting on it.
///
/// SIGTSTP has its default action meanwhile, so Ctrl+Z stops this process
/// along with the program (both are in the terminal's foreground process
/// group) and the shell's `fg` resumes both. A program that stops only itself
/// (`kill(getpid(), SIGTSTP)`) is followed: this process stops its own group
/// too, so the shell regains the terminal rather than waiting on a process that
/// waits on a stopped one, and the program is continued with it. The terminal
/// is already handed back, so a stop here leaves the shell usable. The
/// disposition is put back afterwards.
pub(super) fn run_foreground_child(
    command: &mut std::process::Command,
    quit_requested: impl Fn() -> bool,
) -> io::Result<ExitStatus> {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

    let default = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
    // Safety: the default action runs no code in this process.
    #[allow(unsafe_code)]
    let previous = unsafe { sigaction(Signal::SIGTSTP, &default) }.map_err(io::Error::from)?;
    let status = if quit_requested() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "a quit arrived before it started",
        ))
    } else {
        // The handle is dropped once the pid is read: `wait_for_exit` reaps
        // the program itself, so it can clear the pid first.
        command.spawn().and_then(|child| {
            let pid = i32::try_from(child.id()).unwrap_or(0);
            FOREGROUND_PID.store(pid, Ordering::SeqCst);
            // A quit that arrived between the spawn and the store found no
            // pid to pass on, so it is passed on here.
            if quit_requested() {
                forward_signal(pid, nix::libc::SIGTERM);
            }
            wait_for_exit(pid)
        })
    };
    // Safety: restores the action this process had a moment ago.
    #[allow(unsafe_code)]
    let restored = unsafe { sigaction(Signal::SIGTSTP, &previous) };
    restored.map_err(io::Error::from)?;
    status
}

/// Waits for the foreground program `pid` to exit, following it through any
/// stop, then clears `FOREGROUND_PID` and reaps it, in that order.
fn wait_for_exit(pid: i32) -> io::Result<ExitStatus> {
    use nix::{
        libc,
        sys::{
            signal::{Signal, kill},
            wait::{WaitStatus, waitpid},
        },
        unistd::Pid,
    };
    use std::os::unix::process::ExitStatusExt;

    let child = Pid::from_raw(pid);
    // Reported without reaping, so the pid stays the program's while the
    // handler may still signal it.
    while peek_child(pid, libc::WEXITED | libc::WSTOPPED | libc::WNOWAIT)? == libc::CLD_STOPPED {
        // Consumed only if the program is still stopped: after a Ctrl+Z this
        // process stopped too, and `fg` continued both.
        if peek_child(pid, libc::WSTOPPED | libc::WNOHANG)? == libc::CLD_STOPPED {
            // Stops this process's group, the program included. In an orphaned
            // group the kernel discards it and nothing stops.
            let _ = kill(Pid::from_raw(0), Signal::SIGTSTP);
            let _ = kill(child, Signal::SIGCONT);
        }
    }
    FOREGROUND_PID.store(0, Ordering::SeqCst);
    match waitpid(child, None).map_err(io::Error::from)? {
        WaitStatus::Exited(_, code) => Ok(ExitStatus::from_raw(code << 8)),
        WaitStatus::Signaled(_, signal, core) => Ok(ExitStatus::from_raw(
            signal as i32 | if core { 0x80 } else { 0 },
        )),
        other => Err(io::Error::other(format!("unexpected status {other:?}"))),
    }
}

/// `waitid(2)` on `pid` with `options`, returning the reported `si_code`, or 0
/// when `WNOHANG` found nothing to report. Called directly rather than through
/// nix, whose `waitid` does not exist on macOS.
fn peek_child(pid: i32, options: i32) -> io::Result<i32> {
    use nix::libc;

    let id = libc::id_t::try_from(pid).map_err(io::Error::other)?;
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // Safety: `info` is a zeroed siginfo_t the call fills in, and outlives
        // the call.
        #[allow(unsafe_code)]
        let result = unsafe { libc::waitid(libc::P_PID, id, info.as_mut_ptr(), options) };
        if result == 0 {
            // Safety: zeroed is a valid siginfo_t, and the call filled it in.
            #[allow(unsafe_code)]
            let info = unsafe { info.assume_init() };
            return Ok(info.si_code);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Sends `signal` to the foreground program `pid`, if there is one. Only
/// `kill(2)`, so it is async-signal-safe.
fn forward_signal(pid: i32, signal: i32) {
    if pid > 0 {
        // Safety: kill(2) is async-signal-safe and takes no pointers.
        #[allow(unsafe_code)]
        unsafe {
            nix::libc::kill(pid, signal);
        }
    }
}

/// The signal a quit passes on to the foreground program: a hangup as itself,
/// since the terminal it would read is going away, and SIGTERM for the rest.
/// A program may treat SIGUSR1, SIGUSR2 or SIGALRM as something other than an
/// end (vim does SIGUSR1), which would leave the quit waiting on it.
fn forwarded(signal: i32) -> i32 {
    if signal == nix::libc::SIGHUP {
        signal
    } else {
        nix::libc::SIGTERM
    }
}

/// The handler's part in a quit while a foreground program runs.
fn forward_quit(signal: i32) {
    forward_signal(FOREGROUND_PID.load(Ordering::SeqCst), forwarded(signal));
}

/// Whether the handler lets `signal` pass rather than quitting: the keyboard's
/// interrupt and quit while a foreground child owns the terminal.
fn is_the_childs(signal: i32, child_running: bool) -> bool {
    child_running && (signal == nix::libc::SIGINT || signal == nix::libc::SIGQUIT)
}

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
// fixed address, async-signal-safe per POSIX, as are `write(2)` and `kill(2)`.
extern "C" fn handle_signal(signal: i32) {
    // A failed `write` or `kill` below sets errno, which belongs to whatever
    // the interrupted thread was doing: one between a failed call and reading
    // its error would read this handler's instead.
    keeping_errno(|| answer_signal(signal));
}

/// Runs `act` and puts errno back as it found it.
fn keeping_errno(act: impl FnOnce()) {
    let errno = Errno::last_raw();
    act();
    Errno::set_raw(errno);
}

fn answer_signal(signal: i32) {
    if is_the_childs(signal, FOREGROUND_CHILD.load(Ordering::SeqCst)) {
        return;
    }
    note_quit(signal);

    let fd = SIGNAL_PIPE_WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        // No pipe: only the recorded signal is available, which the reader
        // thread checks.
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

/// Records `signal` as the quit and passes it on to a foreground program.
///
/// Sequentially consistent, with the pid store and the `quit_requested` load in
/// `run_foreground_child`, so either this side sees the pid or the main thread
/// sees the quit: a quit is never missed by both.
fn note_quit(signal: i32) {
    record_first(&QUIT_SIGNAL, signal);
    forward_quit(signal);
}

/// Answers a terminal that hung up as SIGHUP is answered, since no SIGHUP may
/// follow it: the kernel sends one to the session leader, and to this process
/// only when that leader exits, so a leader ignoring SIGHUP (`trap "" HUP`)
/// leaves this process running on a terminal that no longer exists.
fn quit_on_hangup() {
    note_quit(nix::libc::SIGHUP);
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

/// Register handlers for termination signals so the app can exit gracefully,
/// and one for SIGTSTP so that it cannot stop the app with the terminal in raw
/// mode.
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
        for signal in [
            Signal::SIGTERM,
            Signal::SIGINT,
            Signal::SIGHUP,
            Signal::SIGQUIT,
            Signal::SIGUSR1,
            Signal::SIGUSR2,
            Signal::SIGALRM,
        ] {
            sigaction(signal, &action)?;
        }

        let ignore = SigAction::new(
            SigHandler::Handler(ignore_signal),
            nix::sys::signal::SaFlags::SA_RESTART,
            nix::sys::signal::SigSet::empty(),
        );
        sigaction(Signal::SIGTSTP, &ignore)?;
    }
    Ok(())
}

/// Catches a signal only to keep its default action from running. See the
/// SIGTSTP note above for why this is not SIG_IGN.
extern "C" fn ignore_signal(_: i32) {}

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

/// Sends `Command::Quit` once a termination signal has been seen, or the
/// terminal has hung up, independently of the event reader.
///
/// The reader's own check between polls is enough only while the terminal is
/// alive. At EOF `poll` never returns (see `event_loop`), so the reader never
/// reaches its next check and the quit it would read is never seen. A closing
/// terminal is exactly that case: the kernel sends SIGHUP as the pty goes away,
/// or sends nothing when the session leader ignores it, and without this thread
/// the process outlives the terminal as an orphan answering nothing but
/// SIGKILL.
///
/// Nothing on this side can stop the reader spinning. This bounds the spin by
/// how long the process lives, which is until the handler's byte or the
/// hangup arrives.
pub(super) fn spawn_signal_watcher(tx: Sender<Command>) {
    // Not fatal, here or below: the reader still answers signals for a live
    // terminal, which is every case but the one this thread exists for.
    let Some(read_fd) = SIGNAL_PIPE_READ.get() else {
        log::error!("Cannot watch for signals: the self-pipe was not created");
        return;
    };
    let read_fd = read_fd.as_fd();
    let terminal = reader_terminal();

    let builder = thread::Builder::new().name("filectrl-signal-watcher".into());
    let spawn_result = builder.spawn(move || {
        let wake = watch_signal_pipe(read_fd, terminal.as_ref().map(AsFd::as_fd));
        match wake {
            Wake::Signal => (),
            Wake::HangUp => quit_on_hangup(),
            Wake::Stopped => return,
        }
        // A send error means the receiver is already gone, i.e. the app is
        // shutting down by some other route. Nothing left to do either way.
        let _ = tx.send(Command::Quit);
    });

    if let Err(err) = spawn_result {
        log::error!("Failed to spawn signal watcher thread: {err}");
    }
}

/// The terminal the event reader reads, chosen as crossterm chooses it: stdin
/// when it is a terminal, else the controlling terminal. `None` when neither
/// can be opened, which leaves the watcher to signals alone.
fn reader_terminal() -> Option<OwnedFd> {
    use std::io::IsTerminal;

    let stdin = io::stdin();
    if stdin.is_terminal() {
        return stdin.as_fd().try_clone_to_owned().ok();
    }
    std::fs::File::open("/dev/tty").ok().map(OwnedFd::from)
}

/// What ended the watcher's wait.
#[derive(Debug, PartialEq)]
enum Wake {
    /// A handler wrote its byte to the self-pipe.
    Signal,
    /// The terminal hung up.
    HangUp,
    /// The self-pipe closed or failed, so nothing is left to wait on.
    Stopped,
}

/// The watcher's body, over caller-supplied descriptors so tests can drive it
/// through a pipe and a pty of their own. Blocks until a byte arrives or
/// `terminal` hangs up, so a process with no signal pending does no work. The
/// byte's value says nothing; that a handler ran is the whole message.
///
/// No events are asked of the terminal, since its input is the reader's, but a
/// hangup is reported whatever is asked. A terminal that cannot be polled
/// (macOS answers `POLLNVAL` for a terminal device) is dropped from the wait,
/// and the watcher goes on with signals alone.
fn watch_signal_pipe(read_fd: BorrowedFd<'_>, mut terminal: Option<BorrowedFd<'_>>) -> Wake {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

    loop {
        let mut fds = vec![PollFd::new(read_fd, PollFlags::POLLIN)];
        fds.extend(terminal.map(|fd| PollFd::new(fd, PollFlags::empty())));
        match poll(&mut fds, PollTimeout::NONE) {
            // `poll` is never restarted after a handler, SA_RESTART or not.
            // One that ran on this thread has left its byte in the pipe, which
            // the next wait finds at once.
            Ok(_) | Err(Errno::EINTR) => (),
            Err(err) => {
                log::error!("Failed to wait for a signal: {err}");
                return Wake::Stopped;
            }
        }
        let events =
            |fd: Option<&PollFd>| fd.and_then(PollFd::revents).unwrap_or(PollFlags::empty());
        let (pipe, tty) = (events(fds.first()), events(fds.get(1)));
        if !pipe.is_empty() {
            match read_signal_byte(read_fd) {
                Some(wake) => return wake,
                None => continue,
            }
        }
        if tty.contains(PollFlags::POLLNVAL) {
            log::debug!("The terminal cannot be polled for a hangup");
            terminal = None;
        } else if tty.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
            return Wake::HangUp;
        }
    }
}

/// Reads the handler's byte from the self-pipe `poll` found ready. `None` for
/// an interrupted read, which the wait resumes after.
fn read_signal_byte(read_fd: BorrowedFd<'_>) -> Option<Wake> {
    match nix::unistd::read(read_fd, &mut [0u8; 1]) {
        Ok(0) => {
            // Every write end closed. Unreachable while the process lives,
            // since the handler's end is leaked open deliberately.
            log::error!("The signal self-pipe reached end of file");
            Some(Wake::Stopped)
        }
        Ok(_) => Some(Wake::Signal),
        Err(Errno::EINTR) => None,
        Err(err) => {
            log::error!("Failed to read the signal self-pipe: {err}");
            Some(Wake::Stopped)
        }
    }
}

/// Stops the reader thread between polls, so a program run in the foreground
/// reads the terminal alone: a reader still polling would take the child's
/// keystrokes as its own.
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
    /// Asks the reader to stop at its next checkpoint. The reader only reaches
    /// a checkpoint when its poll returns, so the caller wakes it after this
    /// and then calls `wait_paused`: woken before the request, the reader
    /// could pass its checkpoint and go back to a full poll.
    pub(super) fn request_pause(&self) {
        *self.lock() = GateState::PauseRequested;
    }

    /// Waits up to `timeout` for the reader to stop after `request_pause`.
    /// Returns whether it stopped.
    pub(super) fn wait_paused(&self, timeout: Duration) -> bool {
        let state = self.lock();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| *state != GateState::Paused)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state == GateState::Paused
    }

    #[cfg(test)]
    pub(super) fn is_open(&self) -> bool {
        *self.lock() == GateState::Open
    }

    /// Lets the reader poll again.
    pub(super) fn resume(&self) {
        *self.lock() = GateState::Open;
        self.changed.notify_all();
    }

    /// The reader's side: returns at once unless a pause was asked for, in
    /// which case it says so and blocks until `resume`.
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
        // Nothing panics while the lock is held, and every state is valid.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(super) fn spawn_command_sender(tx: &Sender<Command>, gate: Arc<ReaderGate>) {
    // Only the wait for terminal input, so it does not affect how quickly a
    // keystroke is handled. The timeout exists so a recorded quit is checked at
    // all, which matters only when no watcher thread is running.
    let poll_interval = Duration::from_secs(2);

    let builder = thread::Builder::new().name("filectrl-event-reader".into());
    let reader_tx = tx.clone();
    let spawn_result = builder.spawn(move || {
        event_loop(&reader_tx, &gate, poll_interval, &mut TerminalEventSource);
    });

    if let Err(err) = spawn_result {
        log::error!("Failed to spawn event reader thread: {err}");
        // No reader means no terminal input, so the main loop would sit on
        // rx.recv() at a screen that answers nothing until a signal arrives.
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
        // Where a program run in the foreground takes the terminal over.
        gate.checkpoint();

        // Bounds the window between a signal arriving and this thread noticing
        // by the poll timeout. The watcher normally quits first; this answers a
        // signal when it is not running. Checked before the poll rather than
        // after, which also catches a signal that fires between poll() returning
        // Ok(false) and the jump back to the top.
        if quit_requested() {
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

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::mpsc, time::Duration};

    use ratatui::crossterm::event::Event;

    use std::os::{
        fd::{AsFd, BorrowedFd},
        unix::process::ExitStatusExt,
    };

    use nix::{libc, sys::signal::Signal};

    use super::{
        Command, EventSource, FOREGROUND_PID, ReaderGate, SIGNAL_ACTIONS, Wake, event_loop,
        forward_quit, forward_signal, forwarded, handle_signal, ignore_signal,
        install_signal_handlers, is_the_childs, keeping_errno, receive_commands, record_first,
        run_foreground_child, wait_for_exit, watch_signal_pipe,
    };

    const INTERVAL: Duration = Duration::from_millis(500);
    // Long enough that the watcher is blocked in `read` before the byte is
    // written, short enough not to slow the suite down.
    const WRITE_DELAY: Duration = Duration::from_millis(5);

    /// Scripted input. Exhausting the poll script ends `event_loop` through its
    /// production error path, which keeps the tests independent of the global
    /// quit that other tests in this binary share.
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

        // The app is shutting down, so the first undeliverable event ends the
        // loop rather than the reader reading on.
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

        // `recv` fails at once on a disconnected channel, so an empty batch
        // would spin `App::run` instead of ending it.
        assert_eq!(vec![Command::Quit], receive_commands(&rx));
    }

    #[test]
    fn a_byte_already_written_is_a_signal() {
        let (read_fd, write_fd) = nix::unistd::pipe().expect("a pipe should be creatable");
        nix::unistd::write(&write_fd, &[0]).expect("the pipe should accept a byte");

        assert_eq!(Wake::Signal, watch_signal_pipe(read_fd.as_fd(), None));
    }

    /// Runs the watcher over a fresh self-pipe and `terminal`, writing the
    /// handler's byte after `delay`, so a watcher that misses what it should
    /// have woken for still returns (as a signal) rather than hanging.
    fn watch_with_byte_after(terminal: Option<BorrowedFd<'_>>, delay: Duration) -> Wake {
        let (read_fd, write_fd) = nix::unistd::pipe().expect("a pipe should be creatable");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(delay);
                nix::unistd::write(&write_fd, &[0]).expect("the pipe should accept a byte");
            });
            watch_signal_pipe(read_fd.as_fd(), terminal)
        })
    }

    // The watcher must wake on a byte written after it began blocking, which
    // is the real sequence: the handler runs while it is idle.
    #[test]
    fn a_byte_written_while_waiting_is_a_signal() {
        assert_eq!(Wake::Signal, watch_with_byte_after(None, WRITE_DELAY));
    }

    #[test]
    fn a_closed_pipe_stops_the_watcher() {
        let (read_fd, write_fd) = nix::unistd::pipe().expect("a pipe should be creatable");
        // End of file rather than a signal, so nothing was asked for.
        drop(write_fd);

        assert_eq!(Wake::Stopped, watch_signal_pipe(read_fd.as_fd(), None));
    }

    /// A pty whose other side is the terminal emulator: closing the master is
    /// the window closing.
    fn pty() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        let pty = nix::pty::openpty(None, None).expect("a pty should be creatable");
        (pty.master, pty.slave)
    }

    // Linux only: macOS cannot poll a terminal device (see `watch_signal_pipe`).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_terminal_that_hangs_up_wakes_the_watcher() {
        let (master, slave) = pty();
        drop(master);

        assert_eq!(
            Wake::HangUp,
            watch_with_byte_after(Some(slave.as_fd()), Duration::from_secs(1))
        );
    }

    /// Typed input is the reader's, so it must not end the wait.
    #[test]
    fn a_live_terminal_with_input_does_not_wake_the_watcher() {
        let (master, slave) = pty();
        // A whole line, which a terminal in canonical mode makes readable.
        nix::unistd::write(&master, b"q\n").expect("the pty should accept input");

        assert_eq!(
            Wake::Signal,
            watch_with_byte_after(Some(slave.as_fd()), WRITE_DELAY)
        );
    }

    /// A descriptor `poll` answers with `POLLNVAL`, as macOS answers for a
    /// terminal device, leaves the watcher to signals alone.
    #[test]
    fn a_terminal_that_cannot_be_polled_is_not_a_hangup() {
        // Far above any descriptor this process opens, so `poll` finds it
        // closed.
        // Safety: the number is only handed to `poll`, which reports a closed
        // descriptor rather than using it.
        #[allow(unsafe_code)]
        let closed = unsafe { BorrowedFd::borrow_raw(1 << 20) };

        assert_eq!(
            Wake::Signal,
            watch_with_byte_after(Some(closed), WRITE_DELAY)
        );
    }

    /// The signal's current action, read without changing it.
    #[allow(unsafe_code)]
    fn current_action(signal: Signal) -> libc::sigaction {
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // Safety: a null new action only reads the current one into `action`.
        let result =
            unsafe { libc::sigaction(signal as i32, std::ptr::null(), action.as_mut_ptr()) };
        assert_eq!(0, result, "{signal} should have a readable action");
        // Safety: zeroed is a valid `sigaction`, and the call filled it in.
        unsafe { action.assume_init() }
    }

    /// Installed in this process for real, then put back before anything is
    /// asserted, so that the rest of the test binary still ends on SIGINT and
    /// SIGTERM.
    #[test]
    fn the_handlers_are_installed_for_every_signal_they_cover() {
        const TERMINATING: [Signal; 7] = [
            Signal::SIGTERM,
            Signal::SIGINT,
            Signal::SIGHUP,
            Signal::SIGQUIT,
            Signal::SIGUSR1,
            Signal::SIGUSR2,
            Signal::SIGALRM,
        ];
        let _serial = SIGNAL_ACTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let signals: Vec<Signal> = TERMINATING.into_iter().chain([Signal::SIGTSTP]).collect();
        let saved: Vec<libc::sigaction> = signals.iter().map(|&s| current_action(s)).collect();

        let installed = install_signal_handlers();
        let handlers: Vec<libc::sighandler_t> = signals
            .iter()
            .map(|&s| current_action(s).sa_sigaction)
            .collect();
        for (&signal, action) in signals.iter().zip(&saved) {
            // Safety: restores an action this process had a moment ago.
            #[allow(unsafe_code)]
            let result = unsafe { libc::sigaction(signal as i32, action, std::ptr::null_mut()) };
            assert_eq!(0, result, "{signal} should be restored");
        }

        installed.unwrap();
        for (signal, handler) in signals.iter().zip(&handlers) {
            // SIGTSTP is caught by a handler that does nothing, not ignored,
            // which a program launched from the file manager would inherit.
            let expected = if *signal == Signal::SIGTSTP {
                ignore_signal as *const () as libc::sighandler_t
            } else {
                handle_signal as *const () as libc::sighandler_t
            };
            assert_eq!(expected, *handler, "{signal}");
        }
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
                while passes.load(Ordering::SeqCst) < 1_000_000 {
                    gate.checkpoint();
                    passes.fetch_add(1, Ordering::SeqCst);
                    std::thread::yield_now();
                }
            })
        };

        gate.request_pause();
        assert!(gate.wait_paused(Duration::from_secs(5)), "the reader stops");
        let stopped_at = passes.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(stopped_at, passes.load(Ordering::SeqCst));

        gate.resume();
        passes.store(1_000_000, Ordering::SeqCst);
        reader.join().unwrap();
    }

    #[test]
    fn the_first_quit_signal_is_the_one_kept() {
        let slot = std::sync::atomic::AtomicI32::new(0);

        record_first(&slot, libc::SIGTERM);
        record_first(&slot, libc::SIGHUP);

        assert_eq!(
            libc::SIGTERM,
            slot.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    /// The handler's failed `write` or `kill` must not change the errno of
    /// the thread it interrupted.
    #[test]
    fn the_handler_leaves_errno_as_it_found_it() {
        nix::errno::Errno::set_raw(libc::EINTR);

        keeping_errno(|| nix::errno::Errno::set_raw(libc::ESRCH));

        assert_eq!(libc::EINTR, nix::errno::Errno::last_raw());
    }

    #[test]
    fn a_pause_with_no_reader_gives_up() {
        let gate = ReaderGate::default();
        gate.request_pause();
        assert!(!gate.wait_paused(Duration::from_millis(10)));
    }

    /// A quit signal passed to the foreground program ends it, which is what
    /// lets the wait for it return.
    #[test]
    fn a_forwarded_signal_reaches_the_program() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        forward_signal(i32::try_from(child.id()).unwrap(), libc::SIGTERM);

        assert_eq!(Some(libc::SIGTERM), child.wait().unwrap().signal());
    }

    #[test]
    fn no_program_is_signalled_without_a_pid() {
        // Would signal this process's own group if 0 reached kill(2).
        forward_signal(0, libc::SIGTERM);
    }

    /// The disposition in place before a foreground program runs is the one
    /// in place after it.
    #[test]
    fn running_a_foreground_program_puts_the_stop_action_back() {
        let _serial = SIGNAL_ACTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = current_action(Signal::SIGTSTP).sa_sigaction;

        let status =
            run_foreground_child(&mut std::process::Command::new("true"), || false).unwrap();

        assert!(status.success());
        assert_eq!(before, current_action(Signal::SIGTSTP).sa_sigaction);
    }

    /// Starts `sleep 30` as the foreground program on another thread and
    /// returns once its pid is published, with the thread's handle.
    fn sleeping_program() -> (
        i32,
        std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>,
    ) {
        let running = std::thread::spawn(|| {
            run_foreground_child(std::process::Command::new("sleep").arg("30"), || false)
        });
        let pid = loop {
            let pid = FOREGROUND_PID.load(std::sync::atomic::Ordering::SeqCst);
            if pid > 0 {
                break pid;
            }
            std::thread::yield_now();
        };
        (pid, running)
    }

    /// Ctrl+Z stops this process with the program only while SIGTSTP has its
    /// default action, which it must have for as long as the program runs.
    #[test]
    fn the_stop_action_is_the_default_while_a_program_runs() {
        let _serial = SIGNAL_ACTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (pid, running) = sleeping_program();

        let during = current_action(Signal::SIGTSTP).sa_sigaction;
        forward_signal(pid, libc::SIGTERM);
        running.join().unwrap().unwrap();

        assert_eq!(libc::SIG_DFL, during);
    }

    /// The handler's forwarding reaches the running program, which ends the
    /// wait for it.
    #[test]
    fn a_quit_is_passed_on_to_the_running_program() {
        let _serial = SIGNAL_ACTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (_pid, running) = sleeping_program();

        forward_quit(libc::SIGUSR1);
        let status = running.join().unwrap().unwrap();

        // SIGUSR1 went on as SIGTERM, which sleep does not survive.
        assert_eq!(Some(libc::SIGTERM), status.signal());
        assert_eq!(0, FOREGROUND_PID.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test_case::test_case(libc::SIGHUP => libc::SIGHUP ; "a hangup as itself")]
    #[test_case::test_case(libc::SIGTERM => libc::SIGTERM ; "a termination as itself")]
    #[test_case::test_case(libc::SIGUSR1 => libc::SIGTERM ; "a user signal as a termination")]
    #[test_case::test_case(libc::SIGALRM => libc::SIGTERM ; "an alarm as a termination")]
    fn a_quit_reaches_the_program_as(signal: i32) -> i32 {
        forwarded(signal)
    }

    /// A quit that arrived first leaves nothing to start.
    #[test]
    fn a_program_is_not_started_after_a_quit() {
        let _serial = SIGNAL_ACTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let started = std::time::Instant::now();

        let error = run_foreground_child(std::process::Command::new("sleep").arg("30"), || true)
            .unwrap_err();

        assert_eq!(std::io::ErrorKind::Interrupted, error.kind());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test_case::test_case("exit 3" => (Some(3), None) ; "an exit code")]
    #[test_case::test_case("kill -TERM $$" => (None, Some(libc::SIGTERM)) ; "a signal")]
    fn the_status_is_the_programs(script: &str) -> (Option<i32>, Option<i32>) {
        // `wait_for_exit` clears `FOREGROUND_PID`, which the tests running a
        // foreground program wait on under this lock.
        let _serial = SIGNAL_ACTIONS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Reaped by `wait_for_exit`, which is what is under test.
        #[allow(clippy::zombie_processes)]
        let child = std::process::Command::new("sh")
            .args(["-c", script])
            .spawn()
            .unwrap();
        let status = wait_for_exit(i32::try_from(child.id()).unwrap()).unwrap();

        (status.code(), status.signal())
    }

    #[test]
    fn keyboard_signals_are_the_childs_only_while_one_runs() {
        assert!(is_the_childs(libc::SIGINT, true));
        assert!(is_the_childs(libc::SIGQUIT, true));
        assert!(!is_the_childs(libc::SIGTERM, true));
        assert!(!is_the_childs(libc::SIGHUP, true));
        assert!(!is_the_childs(libc::SIGINT, false));
    }
}
