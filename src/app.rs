pub mod clipboard;
pub mod config;
pub mod events;
mod foreground;
mod handler;
pub mod terminal;

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
};

use anyhow::Result;
use ratatui::Frame;

use self::{
    clipboard::Clipboard,
    config::Config,
    events::{
        ReaderGate, quit_requested, receive_commands, spawn_command_sender, spawn_signal_watcher,
    },
    terminal::CleanupOnDropTerminal,
};
use crate::{
    command::{Command, InputMode, handler::CommandHandler, result::CommandResult},
    file_system::{FileSystem, exit_cause, failure_prefix, path_info::compact},
    views::{View, root::RootView},
};

/// The command-handling half of the app, separate from `App` so it can be
/// driven without a terminal.
struct Handlers {
    clipboard: Clipboard,
    file_system: FileSystem,
    root: RootView,
}

/// Resolves each of `commands` and everything it derives, in order, returning
/// what no handler claimed.
fn broadcast_commands(handlers: &mut Handlers, commands: Vec<Command>) -> Vec<Command> {
    let mut unhandled = Vec::new();
    for command in commands {
        let mut pending = VecDeque::from([command]);
        while let Some(command) = pending.pop_front() {
            // Read per command: a derived command may change the mode.
            let mode = handlers.root.mode();
            let mut derived = Vec::new();
            if recursively_handle_command(&mut derived, &command, mode, handlers) {
                pending.extend(derived);
            } else {
                // `derived` is empty here: only a claim can derive.
                unhandled.push(command);
            }
        }
    }
    unhandled
}

pub struct App {
    handlers: Handlers,
    /// Stops the event reader while a program runs in the foreground.
    reader_gate: Arc<ReaderGate>,
    terminal: CleanupOnDropTerminal,
    rx: Receiver<Command>,
    tx: Sender<Command>, // Held to keep the channel open for the lifetime of App
}

impl App {
    pub fn new(terminal: CleanupOnDropTerminal) -> Self {
        let (tx, rx) = mpsc::channel();
        let config = Config::global();
        let handlers = Handlers {
            clipboard: Clipboard::default(),
            file_system: FileSystem::new(config, tx.clone()),
            root: RootView::new(config),
        };
        Self {
            handlers,
            reader_gate: Arc::default(),
            terminal,
            rx,
            tx,
        }
    }

    pub fn run(&mut self, initial_directory: Option<PathBuf>) -> Result<()> {
        // Handled before the loop so the `NavigatedDirectory` registers its generation
        // before the loader's first `ListingBatch`es are drained.
        let initial = self.handlers.file_system.run_once(initial_directory)?;
        warn_unhandled(&broadcast_commands(&mut self.handlers, initial));
        self.render()?;

        spawn_command_sender(&self.tx, self.reader_gate.clone());
        spawn_signal_watcher(self.tx.clone());

        loop {
            let commands = receive_commands(&self.rx);
            let received = commands.len();
            let keys = commands
                .iter()
                .filter(|command| is_key_input(command))
                .count();
            let alerts_mark = self.handlers.root.alerts_mark();

            let remaining_commands = broadcast_commands(&mut self.handlers, commands);
            if claimed_a_key(keys, &remaining_commands) {
                self.handlers.root.expire_alerts_before(alerts_mark);
            }

            if should_quit(&remaining_commands) {
                return Ok(());
            }

            let (foreground, remaining_commands): (Vec<_>, Vec<_>) = remaining_commands
                .into_iter()
                .partition(|command| matches!(command, Command::RunInForeground { .. }));
            warn_unhandled(&remaining_commands);
            for command in foreground {
                if self.run_in_foreground(command)? {
                    return Ok(());
                }
            }
            if changed_nothing_visible(received, &remaining_commands) {
                continue;
            }
            self.render()?;
        }
    }

    /// Runs an editor or pager on an entry, then queues a refresh and any failure
    /// alert. Returns whether a termination signal arrived, in which case the
    /// caller quits. Only a terminal that cannot be taken back is an error.
    fn run_in_foreground(&mut self, command: Command) -> Result<bool> {
        let Command::RunInForeground { program, path } = command else {
            return Ok(false);
        };
        let alert = match foreground::argv(|name| std::env::var_os(name), program, &path.path) {
            Err(error) => Some(Command::AlertWarn(format!("{error:#}"))),
            Ok(argv) => {
                // Built from a variable that was valid UTF-8.
                let program = argv[0].to_string_lossy().into_owned();
                let failure = failure_prefix(&program, &path.path);
                let outcome = foreground::run(
                    &mut self.terminal,
                    &self.reader_gate,
                    foreground::READER_PAUSE_TIMEOUT,
                    &quit_requested,
                    &argv,
                )?;
                match outcome {
                    foreground::Outcome::Ran(Ok(status)) if status.success() => None,
                    foreground::Outcome::Ran(Ok(status)) => Some(Command::AlertError(format!(
                        "{failure}: {}",
                        exit_cause(status)
                    ))),
                    foreground::Outcome::Ran(Err(error)) => {
                        Some(Command::AlertError(format!("{failure}: {error}")))
                    }
                    foreground::Outcome::ReaderBusy => Some(Command::AlertError(format!(
                        "Cannot run {program:?} on {}: the input reader did not stop",
                        compact(&path.path)
                    ))),
                    foreground::Outcome::Quit => return Ok(true),
                }
            }
        };
        let _ = self.tx.send(Command::RefreshDirectory);
        if let Some(alert) = alert {
            let _ = self.tx.send(alert);
        }
        Ok(false)
    }

    fn render(&mut self) -> Result<()> {
        let root = &mut self.handlers.root;
        let theme = Config::global().theme();
        self.terminal.draw(|frame: &mut Frame| {
            let area = frame.area();
            root.render(theme, area, frame);
        })?;
        Ok(())
    }
}

fn recursively_handle_command(
    derived: &mut Vec<Command>,
    command: &Command,
    mode: InputMode,
    handler: &mut dyn CommandHandler,
) -> bool {
    let result = match command {
        Command::Key(code, modifiers) => {
            if handler.should_handle_key(mode) {
                handler.handle_key(*code, *modifiers)
            } else {
                CommandResult::NotHandled
            }
        }
        Command::PasteText(text) => {
            if handler.should_handle_key(mode) {
                handler.handle_paste(text)
            } else {
                CommandResult::NotHandled
            }
        }
        Command::Mouse(mouse_event) => {
            if handler.should_handle_mouse(*mouse_event) {
                handler.handle_mouse(*mouse_event)
            } else {
                CommandResult::NotHandled
            }
        }
        _ => handler.handle_command(command),
    };

    let mut claimed = !matches!(result, CommandResult::NotHandled);
    derived.extend(result.into_commands());

    // A claimed key stops at its handler, so HelpView's scroll keys do not also
    // move the table. Mouse events are not short-circuited: the table takes wheel
    // events over any view.
    let is_key = matches!(command, Command::Key(_, _) | Command::PasteText(_));
    let mut key_consumed = is_key && claimed;
    handler.visit_command_handlers(&mut |child| {
        if key_consumed {
            return;
        }
        let child_handled = recursively_handle_command(derived, command, mode, child);
        claimed |= child_handled;
        if is_key && child_handled {
            key_consumed = true;
        }
    });

    claimed
}

fn warn_unhandled(commands: &[Command]) {
    for command in commands {
        // Unclaimed terminal input is normal; Resize only wakes the render loop.
        if !matches!(
            command,
            Command::Key(_, _) | Command::PasteText(_) | Command::Mouse(_) | Command::Resize { .. }
        ) {
            log::warn!("No handler claimed {command:?}");
        }
    }
}

/// Whether a batch can be drained without redrawing: every command came back
/// unclaimed and is an input event. `handle_key`, `handle_paste` and
/// `handle_mouse` return `NotHandled` only from arms that touch no state.
/// `Resize` is excluded because it exists to redraw.
fn changed_nothing_visible(received: usize, remaining: &[Command]) -> bool {
    !remaining.is_empty()
        && remaining.len() == received
        && remaining.iter().all(|command| {
            matches!(
                command,
                Command::Key(_, _) | Command::PasteText(_) | Command::Mouse(_)
            )
        })
}

fn is_key_input(command: &Command) -> bool {
    matches!(command, Command::Key(_, _) | Command::PasteText(_))
}

/// Whether a handler claimed any of the batch's `keys` key inputs.
fn claimed_a_key(keys: usize, remaining: &[Command]) -> bool {
    remaining
        .iter()
        .filter(|command| is_key_input(command))
        .count()
        < keys
}

fn should_quit(commands: &[Command]) -> bool {
    commands
        .iter()
        .any(|command| matches!(*command, Command::Quit))
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use ratatui::crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use super::*;
    use crate::{
        app::{clipboard::ClipboardEntry, config::Openers},
        test_support::TempDir,
    };

    /// The real handler tree with blank openers (so nothing shells out) and
    /// `config_dir` in `dir`.
    pub(super) fn test_handlers(dir: &TempDir) -> Handlers {
        let mut config = Config::builtin();
        config.config_dir = dir.path().to_path_buf();
        config.openers = Openers {
            open_directory: String::new(),
            open_file: String::new(),
            open_filectrl_window: String::new(),
            run_in_terminal: String::new(),
        };
        let (tx, _rx) = mpsc::channel();
        let file_system = FileSystem::new(&config, tx);
        Config::init_test();
        Handlers {
            clipboard: Clipboard::disabled(),
            file_system,
            root: RootView::new(Config::global()),
        }
    }

    #[test]
    fn a_derived_command_is_broadcast_in_turn() {
        let dir = TempDir::new("app_derived");
        let mut handlers = test_handlers(&dir);
        let entry = ClipboardEntry::Copy(vec![dir.file("file.txt", 1)]);
        handlers.handle_command(&Command::SetClipboardEntry(Some(entry)));

        // Esc derives `ResetView`, which clears the clipboard entry.
        let unhandled = broadcast_commands(
            &mut handlers,
            vec![Command::Key(KeyCode::Esc, KeyModifiers::NONE)],
        );

        assert_eq!(Vec::<Command>::new(), unhandled);
        assert!(matches!(
            Command::try_from(handlers.handle_command(&Command::Paste(dir.directory()))),
            Ok(Command::AlertWarn(_))
        ));
    }

    #[test]
    fn only_unclaimed_commands_are_returned() {
        let dir = TempDir::new("app_unclaimed");
        let mut handlers = test_handlers(&dir);

        let unhandled = broadcast_commands(
            &mut handlers,
            vec![Command::Quit, Command::AlertInfo("x".into()), Command::Quit],
        );

        assert_eq!(vec![Command::Quit, Command::Quit], unhandled);
    }

    /// A handler that logs visits and can consume keys or derive commands.
    struct Spy {
        name: &'static str,
        consume_key: bool,
        accept_mouse: bool,
        /// Derives `.1` in response to `.0`, keyed so a chain terminates.
        derive_on: Option<(Command, Command)>,
        derive_many: Vec<Command>,
        log: Rc<RefCell<Vec<&'static str>>>,
        children: Vec<Spy>,
    }

    impl Spy {
        fn new(name: &'static str, log: &Rc<RefCell<Vec<&'static str>>>) -> Self {
            Self {
                name,
                consume_key: false,
                accept_mouse: false,
                derive_on: None,
                derive_many: Vec::new(),
                log: log.clone(),
                children: Vec::new(),
            }
        }
    }

    impl CommandHandler for Spy {
        fn visit_command_handlers(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
            for child in &mut self.children {
                visitor(child);
            }
        }

        fn handle_command(&mut self, command: &Command) -> CommandResult {
            self.log.borrow_mut().push(self.name);
            if !self.derive_many.is_empty() {
                return CommandResult::HandledWithMany(self.derive_many.clone());
            }
            match &self.derive_on {
                Some((trigger, derived)) if trigger == command => derived.clone().into(),
                _ => CommandResult::NotHandled,
            }
        }

        fn handle_key(&mut self, _code: KeyCode, _modifiers: KeyModifiers) -> CommandResult {
            self.log.borrow_mut().push(self.name);
            if self.consume_key {
                CommandResult::Handled
            } else {
                CommandResult::NotHandled
            }
        }

        fn handle_mouse(&mut self, _event: MouseEvent) -> CommandResult {
            self.log.borrow_mut().push(self.name);
            CommandResult::Handled
        }

        fn should_handle_mouse(&self, _event: MouseEvent) -> bool {
            self.accept_mouse
        }
    }

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn key_dispatch_short_circuits_after_first_handler() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut root = Spy::new("root", &log);
        let mut a = Spy::new("a", &log);
        a.consume_key = true;
        let mut b = Spy::new("b", &log);
        b.consume_key = true;
        root.children = vec![a, b];

        let mut derived = Vec::new();
        let handled = recursively_handle_command(
            &mut derived,
            &Command::Key(KeyCode::Char('x'), KeyModifiers::NONE),
            InputMode::Normal,
            &mut root,
        );

        assert!(handled);
        assert_eq!(vec!["root", "a"], *log.borrow());
    }

    #[test]
    fn a_key_the_parent_claims_does_not_reach_its_children() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut root = Spy::new("root", &log);
        root.consume_key = true;
        let mut a = Spy::new("a", &log);
        a.consume_key = true;
        root.children = vec![a];

        let mut derived = Vec::new();
        recursively_handle_command(
            &mut derived,
            &Command::Key(KeyCode::Char('x'), KeyModifiers::NONE),
            InputMode::Normal,
            &mut root,
        );

        assert_eq!(vec!["root"], *log.borrow());
    }

    #[test]
    fn a_key_skips_every_handler_that_does_not_take_keys_in_the_mode() {
        let log = Rc::new(RefCell::new(Vec::new()));
        // The default `should_handle_key` takes keys in Normal mode only.
        let mut root = Spy::new("root", &log);
        root.consume_key = true;
        root.children = vec![Spy::new("a", &log)];

        let mut derived = Vec::new();
        let handled = recursively_handle_command(
            &mut derived,
            &Command::Key(KeyCode::Char('x'), KeyModifiers::NONE),
            InputMode::Prompt,
            &mut root,
        );

        assert!(!handled);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn a_mouse_event_reaches_every_handler_that_accepts_it() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut root = Spy::new("root", &log);
        let mut a = Spy::new("a", &log);
        a.accept_mouse = true;
        let mut b = Spy::new("b", &log);
        b.accept_mouse = true;
        root.children = vec![a, b];

        let mut derived = Vec::new();
        let handled = recursively_handle_command(
            &mut derived,
            &Command::Mouse(mouse(MouseEventKind::ScrollDown)),
            InputMode::Normal,
            &mut root,
        );

        // The second is not skipped for the first having claimed it.
        assert!(handled);
        assert_eq!(vec!["a", "b"], *log.borrow());
    }

    #[test]
    fn non_key_command_is_broadcast_to_all_handlers() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut root = Spy::new("root", &log);
        root.children = vec![Spy::new("a", &log), Spy::new("b", &log)];

        let mut derived = Vec::new();
        let handled = recursively_handle_command(
            &mut derived,
            &Command::SearchTick,
            InputMode::Normal,
            &mut root,
        );

        assert!(!handled);
        assert_eq!(vec!["root", "a", "b"], *log.borrow());
    }

    #[test]
    fn a_non_key_command_reaches_later_siblings_even_after_one_claims_it() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut root = Spy::new("root", &log);
        let mut a = Spy::new("a", &log);
        a.derive_on = Some((Command::SearchTick, Command::ResetView));
        root.children = vec![a, Spy::new("b", &log)];

        let mut derived = Vec::new();
        let handled = recursively_handle_command(
            &mut derived,
            &Command::SearchTick,
            InputMode::Normal,
            &mut root,
        );

        // Only a key short-circuits: `SelectionChanged` is read by two views.
        assert!(handled);
        assert_eq!(vec!["root", "a", "b"], *log.borrow());
        assert_eq!(vec![Command::ResetView], derived);
    }

    #[test]
    fn handled_with_pushes_derived_command() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut root = Spy::new("root", &log);
        root.derive_on = Some((Command::SearchTick, Command::Quit));

        let mut derived = Vec::new();
        let handled = recursively_handle_command(
            &mut derived,
            &Command::SearchTick,
            InputMode::Normal,
            &mut root,
        );

        assert!(handled);
        assert_eq!(vec![Command::Quit], derived);
    }

    #[test]
    fn handled_with_many_pushes_all_derived_commands() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut root = Spy::new("root", &log);
        root.derive_many = vec![Command::CancelTask, Command::Quit];

        let mut derived = Vec::new();
        let handled = recursively_handle_command(
            &mut derived,
            &Command::SearchTick,
            InputMode::Normal,
            &mut root,
        );

        assert!(handled);
        assert_eq!(vec![Command::CancelTask, Command::Quit], derived);
    }

    #[test]
    fn maybe_from_maps_terminal_events() {
        assert_eq!(
            Some(Command::Key(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Command::maybe_from(&Event::Key(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::CONTROL
            )))
        );
        assert_eq!(
            Some(Command::Resize {
                width: 10,
                height: 20
            }),
            Command::maybe_from(&Event::Resize(10, 20))
        );
        assert!(matches!(
            Command::maybe_from(&Event::Mouse(mouse(MouseEventKind::Down(
                MouseButton::Left
            )))),
            Some(Command::Mouse(_))
        ));
        assert_eq!(
            None,
            Command::maybe_from(&Event::Mouse(mouse(MouseEventKind::Moved)))
        );
        assert_eq!(
            Some(Command::PasteText("a\nb".into())),
            Command::maybe_from(&Event::Paste("a\nb".into()))
        );
        assert_eq!(None, Command::maybe_from(&Event::FocusGained));
    }

    #[test]
    fn only_a_claimed_key_counts() {
        let key = Command::Key(KeyCode::Char('~'), KeyModifiers::NONE);
        let click = Command::Mouse(mouse(MouseEventKind::Down(MouseButton::Left)));

        assert!(claimed_a_key(1, &[]));
        assert!(claimed_a_key(2, std::slice::from_ref(&key)));
        assert!(!claimed_a_key(1, std::slice::from_ref(&key)));
        assert!(!claimed_a_key(0, &[click]));
        assert!(!claimed_a_key(0, &[]));
    }

    #[test]
    fn a_batch_of_unclaimed_input_skips_the_render() {
        let key = Command::Key(KeyCode::Char('~'), KeyModifiers::NONE);
        let click = Command::Mouse(mouse(MouseEventKind::Down(MouseButton::Left)));

        assert!(changed_nothing_visible(2, &[key.clone(), click]));
        assert!(!changed_nothing_visible(0, &[]));
        assert!(!changed_nothing_visible(2, std::slice::from_ref(&key)));
        assert!(!changed_nothing_visible(
            2,
            &[
                key,
                Command::Resize {
                    width: 1,
                    height: 1
                }
            ]
        ));
    }

    #[test]
    fn should_quit_detects_quit_command() {
        assert!(should_quit(&[
            Command::AlertInfo("x".into()),
            Command::Quit
        ]));
        assert!(!should_quit(&[Command::AlertInfo("x".into())]));
        assert!(!should_quit(&[]));
    }
}
