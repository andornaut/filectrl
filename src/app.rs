pub mod clipboard;
pub mod config;
#[cfg(debug_assertions)]
mod debug;
pub mod events;
mod handler;
pub mod terminal;

use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
};

use anyhow::{Result, anyhow};
use ratatui::Frame;

use self::{
    clipboard::Clipboard,
    config::Config,
    events::{receive_commands, spawn_command_sender, spawn_signal_watcher},
    terminal::CleanupOnDropTerminal,
};
use crate::{
    command::{Command, InputMode, handler::CommandHandler, result::CommandResult},
    file_system::FileSystem,
    views::{View, root::RootView},
};

/// Maximum number of broadcast cycles per input command. Each resolves one link
/// in an intent → result chain, the longest of which is the 6 below, from
/// renaming a bookmark while the bookmarks view is showing (`Chmod` and
/// `CreateDirectory` from that view take the same shape):
///
///   1. `Key`                - terminal input
///   2. `Rename`             - submitted by the prompt
///   3. `RefreshedDirectory` - `FileSystem` renames, then refreshes the CWD
///   4. `GetBookmarks`       - `TableView` reloads the shown bookmarks list
///   5. `Bookmarks`          - result emitted by `FileSystem`
///   6. `SelectionChanged`   - `TableView` re-sorts and selects the top entry
///
/// `RefreshedDirectory`/`NavigatedDirectory` only switch the directory; the
/// entries stream in afterwards as `ListingBatch`/`DirectoryListingComplete`,
/// fresh channel sends that each start their own short chain rather than
/// extending this one.
///
/// The bound keeps one cycle of headroom over that, and guards against a handler
/// stuck deriving forever; see `broadcast_command` for what exceeding it does.
const MAX_BROADCAST_CHAIN_LENGTH: u8 = 7;

/// The command-handling half of the app: the whole tree a broadcast visits.
/// Split from `App` so it can be built and driven without a terminal, which
/// is what lets the broadcast be exercised in tests.
struct Handlers {
    clipboard: Clipboard,
    #[cfg(debug_assertions)]
    debug: debug::DebugHandler,
    file_system: FileSystem,
    root: RootView,
}

/// A handler tree the broadcast loop can drive: the tree itself, plus where to
/// read the input mode from. Only `Handlers` implements it in production; the
/// trait exists so the loop's own behavior (chaining, the cycle limit) can be
/// tested against a handler that does nothing else.
trait Broadcast: CommandHandler {
    fn mode(&self) -> InputMode;
}

impl Broadcast for Handlers {
    fn mode(&self) -> InputMode {
        self.root.mode()
    }
}

fn broadcast_commands<H: Broadcast>(
    handlers: &mut H,
    tx: &Sender<Command>,
    commands: Vec<Command>,
) -> Vec<Command> {
    commands
        .into_iter()
        .flat_map(|command| broadcast_command(handlers, tx, command))
        .collect()
}

/// Resolves `command` and everything it derives, returning what no handler
/// claimed. See `MAX_BROADCAST_CHAIN_LENGTH` for the bound and why it exists.
fn broadcast_command<H: Broadcast>(
    handlers: &mut H,
    tx: &Sender<Command>,
    command: Command,
) -> Vec<Command> {
    let mut pending = vec![command];
    let mut unhandled = Vec::new();

    for _ in 0..MAX_BROADCAST_CHAIN_LENGTH {
        if pending.is_empty() {
            break;
        }
        // Re-read mode each iteration so a derived command that changes mode
        // (e.g. OpenPrompt) is reflected in subsequent cycles.
        let mode = handlers.mode();
        let mut next_pending = Vec::new();
        for cmd in pending {
            let mut derived = Vec::new();
            let handled = recursively_handle_command(&mut derived, &cmd, mode, handlers);
            if handled {
                // Only derived commands (HandledWith) continue to the next cycle.
                next_pending.append(&mut derived);
            } else {
                // Unhandled commands are returned as-is; never re-queued.
                // `derived` is necessarily empty here: a handler only pushes to
                // it via `HandledWith`/`HandledWithMany`, which force
                // `handled == true`.
                unhandled.push(cmd);
            }
        }
        pending = next_pending;
    }

    if !pending.is_empty() {
        // A chain longer than expected, or a handler stuck deriving in a
        // loop; both bugs. Loud in dev/test, and in release an alert through
        // the channel rather than silently dropping the user's action.
        let message = format!(
            "Broadcast cycle limit ({MAX_BROADCAST_CHAIN_LENGTH}) exceeded; dropped {} derived command(s): {:?}",
            pending.len(),
            pending
        );
        log::error!("{message}");
        let _ = tx.send(Command::AlertError(message.clone()));
        debug_assert!(false, "{message}");
    }

    unhandled
}

pub struct App {
    handlers: Handlers,
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
            #[cfg(debug_assertions)]
            debug: debug::DebugHandler,
            file_system: FileSystem::new(config, tx.clone()),
            root: RootView::new(),
        };
        Self {
            handlers,
            terminal,
            rx,
            tx,
        }
    }

    pub fn run(&mut self, initial_directory: Option<PathBuf>) -> Result<()> {
        // Handled synchronously, before the loop: `run_once` spawns the loader,
        // which starts streaming `ListingBatch`es at once, and handling the
        // resulting `NavigatedDirectory` here registers its generation before
        // those batches are drained, so none are dropped.
        let initial = self.handlers.file_system.run_once(initial_directory)?;
        let remaining = broadcast_commands(&mut self.handlers, &self.tx, initial);
        must_not_contain_unhandled(&remaining)?;
        self.render()?;

        spawn_command_sender(&self.tx);
        // Answers termination signals when the reader thread cannot, which is
        // the case whenever the terminal itself is what went away.
        spawn_signal_watcher(self.tx.clone());

        loop {
            let commands = receive_commands(&self.rx);
            let received = commands.len();

            let remaining_commands = broadcast_commands(&mut self.handlers, &self.tx, commands);

            if should_quit(&remaining_commands) {
                return Ok(());
            }

            must_not_contain_unhandled(&remaining_commands)?;
            if changed_nothing_visible(received, &remaining_commands) {
                continue;
            }
            self.render()?;
        }
    }

    fn render(&mut self) -> Result<()> {
        let root = &mut self.handlers.root;
        self.terminal.draw(|frame: &mut Frame| {
            let area = frame.area();
            root.render(area, frame);
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
    // Sibling commands are queued for the same next cycle, so deriving
    // several does not lengthen the chain.
    derived.extend(result.into_commands());

    // Once a handler claims a key, siblings are skipped, so HelpView's scroll
    // keys do not also move the table selection. Mouse events are deliberately
    // not short-circuited: a positional click already reaches at most one handler
    // (views occupy disjoint regions), but TableView accepts scroll-wheel events
    // wherever the cursor is, so a wheel event over another view must reach both,
    // and short-circuiting would make that depend on sibling order. Non-key
    // commands are always broadcast to every handler.
    let is_key = matches!(command, Command::Key(_, _));
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

// Terminal events that may go unhandled without error:
// - Key/Mouse: not all inputs are bound to actions
// - Resize: wakes the render loop; ratatui redraws automatically
fn is_ignorable_unhandled(command: &Command) -> bool {
    matches!(
        command,
        Command::Key(_, _) | Command::Mouse(_) | Command::Resize { .. }
    )
}

fn must_not_contain_unhandled(commands: &[Command]) -> Result<()> {
    let unhandled: Vec<_> = commands
        .iter()
        .filter(|command| !is_ignorable_unhandled(command))
        .collect();
    if !unhandled.is_empty() {
        return Err(anyhow!(
            "Unhandled {} command(s): {:?}",
            unhandled.len(),
            unhandled
        ));
    }
    Ok(())
}

/// Whether a batch can be drained without redrawing: the same number of commands
/// came back as went out, all unclaimed, and every one is an input event.
///
/// A claimed command may have changed the screen, so any batch holding one
/// redraws. An unclaimed input event cannot have: `handle_key` and `handle_mouse`
/// return `NotHandled` only from arms that touch no state, which is what an
/// unbound keystroke or a click landing on no view hits.
///
/// `Resize` is excluded, though it too goes unhandled: it is the notification
/// that the dimensions changed, and exists to redraw.
fn changed_nothing_visible(received: usize, remaining: &[Command]) -> bool {
    !remaining.is_empty()
        && remaining.len() == received
        && remaining
            .iter()
            .all(|command| matches!(command, Command::Key(_, _) | Command::Mouse(_)))
}

fn should_quit(commands: &[Command]) -> bool {
    commands
        .iter()
        .any(|command| matches!(*command, Command::Quit))
}

#[cfg(test)]
mod claims;

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use ratatui::crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use super::*;

    /// A `CommandHandler` that records the order in which it is visited and can
    /// be configured to consume keys or to derive a follow-up command.
    struct Spy {
        name: &'static str,
        consume_key: bool,
        /// Derives `.1` in response to `.0`. Keyed on the incoming command
        /// rather than deriving unconditionally, so a chain driven through
        /// `broadcast_command` terminates instead of feeding itself forever.
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
        // root is visited (and declines), a consumes the key, b is skipped.
        assert_eq!(vec!["root", "a"], *log.borrow());
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

        assert!(!handled); // none of the spies handle it
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

        // Only a key short-circuits. A claimed non-key command must still
        // reach the rest of the tree: `SelectionChanged` is read by both
        // NoticesView and StatusView, and whichever came second would stop
        // seeing it.
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

    impl Broadcast for Spy {
        fn mode(&self) -> InputMode {
            InputMode::Normal
        }
    }

    /// A `Spy` and the channel the cycle-limit alert would go out on.
    fn broadcaster(
        log: &Rc<RefCell<Vec<&'static str>>>,
    ) -> (Spy, mpsc::Sender<Command>, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel();
        (Spy::new("root", log), tx, rx)
    }

    #[test]
    fn a_derived_command_is_broadcast_in_turn() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut root, tx, _rx) = broadcaster(&log);
        root.derive_on = Some((Command::SearchTick, Command::ResetView));

        let unhandled = broadcast_command(&mut root, &tx, Command::SearchTick);

        // Two cycles: the input, then what it derived. Resolving an intent
        // into a result is the whole reason the loop exists, and a handler
        // that only ran the first cycle would leave the result unhandled but
        // never delivered.
        assert_eq!(vec!["root", "root"], *log.borrow());
        assert_eq!(vec![Command::ResetView], unhandled);
    }

    #[test]
    fn an_unclaimed_command_is_returned_rather_than_re_queued() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut root, tx, _rx) = broadcaster(&log);

        let unhandled = broadcast_command(&mut root, &tx, Command::Quit);

        // Visited once and handed back. `should_quit` reads this list, so a
        // command re-queued here would spin the loop instead of exiting, and
        // one dropped would stop the app from ever quitting.
        assert_eq!(vec!["root"], *log.borrow());
        assert_eq!(vec![Command::Quit], unhandled);
    }

    #[test]
    fn a_chain_is_bounded_by_the_cycle_limit() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut root, tx, rx) = broadcaster(&log);
        // A handler that answers every SearchTick with another one: the
        // stuck-deriving bug the limit exists to stop.
        root.derive_on = Some((Command::SearchTick, Command::SearchTick));

        let unhandled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            broadcast_command(&mut root, &tx, Command::SearchTick)
        }));

        // `debug_assert!` makes it a panic in a debug build, which is where a
        // developer will see it; the release path is the alert below.
        assert!(unhandled.is_err(), "the cycle limit should have asserted");
        assert_eq!(
            MAX_BROADCAST_CHAIN_LENGTH as usize,
            log.borrow().len(),
            "the loop must stop at the limit rather than run on"
        );
        let Ok(Command::AlertError(message)) = rx.try_recv() else {
            panic!("the user must be told rather than silently losing the action")
        };
        assert!(message.contains("Broadcast cycle limit"), "{message}");
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
        // Moved is suppressed; non-terminal events are ignored.
        assert_eq!(
            None,
            Command::maybe_from(&Event::Mouse(mouse(MouseEventKind::Moved)))
        );
        assert_eq!(None, Command::maybe_from(&Event::FocusGained));
    }

    #[test]
    fn ignorable_unhandled_only_for_terminal_input() {
        assert!(is_ignorable_unhandled(&Command::Key(
            KeyCode::Esc,
            KeyModifiers::NONE
        )));
        assert!(is_ignorable_unhandled(&Command::Mouse(mouse(
            MouseEventKind::Moved
        ))));
        assert!(is_ignorable_unhandled(&Command::Resize {
            width: 1,
            height: 1
        }));
        assert!(!is_ignorable_unhandled(&Command::Quit));
        assert!(!is_ignorable_unhandled(&Command::AlertInfo("x".into())));
    }

    #[test]
    fn must_not_contain_unhandled_rejects_non_ignorable() {
        assert!(
            must_not_contain_unhandled(&[
                Command::Key(KeyCode::Esc, KeyModifiers::NONE),
                Command::Resize {
                    width: 1,
                    height: 1
                },
            ])
            .is_ok()
        );
        assert!(must_not_contain_unhandled(&[]).is_ok());
        assert!(must_not_contain_unhandled(&[Command::AlertInfo("x".into())]).is_err());
    }

    #[test]
    fn a_batch_of_unclaimed_input_skips_the_render() {
        let key = Command::Key(KeyCode::Char('~'), KeyModifiers::NONE);
        let click = Command::Mouse(mouse(MouseEventKind::Down(MouseButton::Left)));

        assert!(changed_nothing_visible(2, &[key.clone(), click]));
        assert!(!changed_nothing_visible(0, &[]));
        // One of the two was claimed, so the batch may have changed the screen.
        assert!(!changed_nothing_visible(2, std::slice::from_ref(&key)));
        // A resize is unhandled by design and is precisely a reason to redraw.
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
