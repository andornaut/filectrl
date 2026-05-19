pub mod clipboard;
pub mod config;
#[cfg(debug_assertions)]
mod debug;
mod events;
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
    events::{receive_commands, spawn_command_sender},
    terminal::CleanupOnDropTerminal,
};
use crate::{
    command::{Command, InputMode, handler::CommandHandler, result::CommandResult},
    file_system::FileSystem,
    views::{View, root::RootView},
};

/// Maximum number of broadcast cycles per input command. Each cycle resolves
/// one link in an intent → result chain; the longest legitimate chain is 4:
///
///   1. `Key`                          — terminal input
///   2. `GoToParentDirectory` / `Open` — navigation intent derived from the key
///   3. `NavigatedDirectory`           — result emitted by `FileSystem`
///   4. `SelectionChanged`             — emitted by `TableView` for the new listing
///
/// Also acts as a guard against a handler stuck deriving commands forever.
/// See `broadcast_command` for what happens when it is exceeded.
const BROADCASTS_COUNT: u8 = 4;

pub struct App {
    clipboard: Clipboard,
    #[cfg(debug_assertions)]
    debug: debug::DebugHandler,
    file_system: FileSystem,
    root: RootView,
    terminal: CleanupOnDropTerminal,
    rx: Receiver<Command>,
    tx: Sender<Command>, // Held to keep the channel open for the lifetime of App
}

impl App {
    pub fn new(terminal: CleanupOnDropTerminal) -> Self {
        let (tx, rx) = mpsc::channel();
        let config = Config::global();
        let clipboard = Clipboard::default();
        let file_system = FileSystem::new(config, tx.clone());
        let root = RootView::new();
        Self {
            clipboard,
            #[cfg(debug_assertions)]
            debug: debug::DebugHandler,
            file_system,
            root,
            terminal,
            rx,
            tx,
        }
    }

    pub fn run(&mut self, initial_directory: Option<PathBuf>) -> Result<()> {
        // An initial command is required to start the main loop
        self.tx
            .send(self.file_system.run_once(initial_directory)?)?;

        spawn_command_sender(self.tx.clone());

        loop {
            let commands = receive_commands(&self.rx);

            let remaining_commands = self.broadcast_commands(commands);

            if should_quit(&remaining_commands) {
                return Ok(());
            }

            must_not_contain_unhandled(&remaining_commands)?;
            self.render()?;
        }
    }

    fn broadcast_commands(&mut self, commands: Vec<Command>) -> Vec<Command> {
        commands
            .into_iter()
            .flat_map(|command| self.broadcast_command(command))
            .collect()
    }

    fn broadcast_command(&mut self, command: Command) -> Vec<Command> {
        let mut pending = vec![command];
        let mut unhandled = Vec::new();

        for _ in 0..BROADCASTS_COUNT {
            if pending.is_empty() {
                break;
            }
            // Re-read mode each iteration so a derived command that changes mode
            // (e.g. OpenPrompt) is reflected in subsequent cycles.
            let mode = self.root.mode();
            let mut next_pending = Vec::new();
            for cmd in pending {
                let mut derived = Vec::new();
                let handled = recursively_handle_command(&mut derived, &cmd, &mode, self);
                if handled {
                    // Only derived commands (HandledWith) continue to the next cycle.
                    next_pending.append(&mut derived);
                } else {
                    // Unhandled commands are returned as-is; never re-queued.
                    // `derived` is necessarily empty here: a handler only pushes to
                    // it via `HandledWith`, which forces `handled == true`.
                    unhandled.push(cmd);
                }
            }
            pending = next_pending;
        }

        if !pending.is_empty() {
            // A non-empty `pending` after the cap means either a chain longer
            // than expected or a handler stuck deriving in a loop — both bugs.
            // Fail loudly in dev/test; in release surface an alert (sent through
            // the channel so it is non-fatal) instead of silently dropping the
            // user's action.
            let message = format!(
                "Broadcast cycle limit ({BROADCASTS_COUNT}) exceeded; dropped {} derived command(s): {:?}",
                pending.len(),
                pending
            );
            log::error!("{message}");
            let _ = self.tx.send(Command::AlertError(message.clone()));
            debug_assert!(false, "{message}");
        }

        unhandled
    }

    fn render(&mut self) -> Result<()> {
        self.terminal.draw(|frame: &mut Frame| {
            let area = frame.area();
            self.root.render(area, frame);
        })?;
        Ok(())
    }
}

fn recursively_handle_command(
    derived: &mut Vec<Command>,
    command: &Command,
    mode: &InputMode,
    handler: &mut dyn CommandHandler,
) -> bool {
    let result = match command {
        Command::Key(code, modifiers) => {
            if handler.should_handle_key(mode) {
                handler.handle_key(code, modifiers)
            } else {
                CommandResult::NotHandled
            }
        }
        Command::Mouse(mouse_event) => {
            if handler.should_handle_mouse(mouse_event) {
                handler.handle_mouse(mouse_event)
            } else {
                CommandResult::NotHandled
            }
        }
        _ => handler.handle_command(command),
    };

    let mut handled = !matches!(result, CommandResult::NotHandled);

    if let CommandResult::HandledWith(derived_command) = result {
        derived.push(*derived_command);
    }

    // Short-circuit key dispatch: once one handler claims a key, siblings are skipped.
    // This prevents, e.g., HelpView's scroll keys from also moving the table selection.
    // Mouse events are deliberately NOT short-circuited. Positional clicks already reach
    // at most one handler (visible views occupy disjoint layout regions), but scroll-wheel
    // events are accepted by TableView regardless of cursor position, so a wheel event over
    // another view's region must reach both. Short-circuiting would make that depend on
    // sibling order.
    // Non-key commands (NavigatedDirectory, ResetView, …) are always broadcast to all handlers.
    let is_key = matches!(command, Command::Key(_, _));
    let mut key_consumed = is_key && handled;
    handler.visit_command_handlers(&mut |child| {
        if key_consumed {
            return;
        }
        let child_handled = recursively_handle_command(derived, command, mode, child);
        handled |= child_handled;
        if is_key && child_handled {
            key_consumed = true;
        }
    });

    handled
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

    /// A `CommandHandler` that records the order in which it is visited and can
    /// be configured to consume keys or to derive a follow-up command.
    struct Spy {
        name: &'static str,
        consume_key: bool,
        derive: Option<Command>,
        log: Rc<RefCell<Vec<&'static str>>>,
        children: Vec<Spy>,
    }

    impl Spy {
        fn new(name: &'static str, log: &Rc<RefCell<Vec<&'static str>>>) -> Self {
            Self {
                name,
                consume_key: false,
                derive: None,
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

        fn handle_command(&mut self, _command: &Command) -> CommandResult {
            self.log.borrow_mut().push(self.name);
            match &self.derive {
                Some(command) => command.clone().into(),
                None => CommandResult::NotHandled,
            }
        }

        fn handle_key(&mut self, _code: &KeyCode, _modifiers: &KeyModifiers) -> CommandResult {
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
            &InputMode::Normal,
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
            &Command::ResetHelpScroll,
            &InputMode::Normal,
            &mut root,
        );

        assert!(!handled); // none of the spies handle it
        assert_eq!(vec!["root", "a", "b"], *log.borrow());
    }

    #[test]
    fn handled_with_pushes_derived_command() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut root = Spy::new("root", &log);
        root.derive = Some(Command::Quit);

        let mut derived = Vec::new();
        let handled = recursively_handle_command(
            &mut derived,
            &Command::ResetHelpScroll,
            &InputMode::Normal,
            &mut root,
        );

        assert!(handled);
        assert_eq!(vec![Command::Quit], derived);
    }

    #[test]
    fn maybe_from_maps_terminal_events() {
        assert_eq!(
            Some(Command::Key(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Command::maybe_from(Event::Key(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::CONTROL
            )))
        );
        assert_eq!(
            Some(Command::Resize {
                width: 10,
                height: 20
            }),
            Command::maybe_from(Event::Resize(10, 20))
        );
        assert!(matches!(
            Command::maybe_from(Event::Mouse(mouse(MouseEventKind::Down(MouseButton::Left)))),
            Some(Command::Mouse(_))
        ));
        // Moved is suppressed; non-terminal events are ignored.
        assert_eq!(
            None,
            Command::maybe_from(Event::Mouse(mouse(MouseEventKind::Moved)))
        );
        assert_eq!(None, Command::maybe_from(Event::FocusGained));
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
    fn should_quit_detects_quit_command() {
        assert!(should_quit(&[
            Command::AlertInfo("x".into()),
            Command::Quit
        ]));
        assert!(!should_quit(&[Command::AlertInfo("x".into())]));
        assert!(!should_quit(&[]));
    }
}
