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

const BROADCASTS_COUNT: u8 = 4; // Max chain depth: Key → Open → NavigateDirectory/RefreshDirectory → SetSelected

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
            let mut derived = Vec::new();
            for cmd in pending {
                let handled = recursively_handle_command(&mut derived, &cmd, &mode, self);
                if handled {
                    // Only derived commands (HandledWith) continue to the next cycle.
                    next_pending.append(&mut derived);
                } else {
                    // Unhandled commands are returned as-is; never re-queued.
                    derived.clear();
                    unhandled.push(cmd);
                }
            }
            pending = next_pending;
        }

        if !pending.is_empty() {
            log::error!(
                "Broadcast cycle limit ({BROADCASTS_COUNT}) exceeded; dropping {} derived command(s): {:?}",
                pending.len(),
                pending
            );
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
    // Non-key commands (NavigateDirectory, Reset, …) are always broadcast to all handlers.
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
