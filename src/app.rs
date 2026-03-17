pub mod config;
#[cfg(debug_assertions)]
mod debug;
mod events;
pub mod state;
pub mod terminal;

use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
};

use anyhow::{Result, anyhow};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use self::{
    config::Config,
    events::{receive_commands, spawn_command_sender},
    state::AppState,
    terminal::CleanupOnDropTerminal,
};
use crate::{
    clipboard::ClipboardCommand,
    command::{Command, handler::CommandHandler, mode::InputMode, result::CommandResult},
    file_system::FileSystem,
    views::{View, root::RootView},
};

const BROADCASTS_COUNT: u8 = 2;

pub struct App {
    config: Config,
    #[cfg(debug_assertions)]
    debug: debug::DebugHandler,
    file_system: FileSystem,
    is_truecolor: bool,
    root: RootView,
    state: AppState,
    terminal: CleanupOnDropTerminal,
    rx: Receiver<Command>,
    tx: Sender<Command>,
}

impl App {
    pub fn new(config: Config, terminal: CleanupOnDropTerminal, is_truecolor: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let file_system = FileSystem::new(&config, tx.clone());
        let root = RootView::new(&config);
        Self {
            config,
            #[cfg(debug_assertions)]
            debug: debug::DebugHandler::default(),
            file_system,
            state: AppState::new(),
            root,
            terminal,
            is_truecolor,
            rx,
            tx,
        }
    }

    pub fn run_once(&mut self, initial_directory: Option<PathBuf>) -> Result<()> {
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
            let mode = self.state.mode.clone();
            let mut next_pending = Vec::new();
            for cmd in pending {
                let (derived, handled) = recursively_handle_command(self, &cmd, &mode);
                if handled {
                    // Only derived commands (HandledWith) continue to the next cycle.
                    next_pending.extend(derived);
                } else {
                    // Unhandled commands are returned as-is; never re-queued.
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
            let theme = if self.is_truecolor {
                &self.config.theme
            } else {
                &self.config.theme256
            };
            self.root.render(area, frame, &self.state, theme);
        })?;
        Ok(())
    }
}

impl CommandHandler for App {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        let file_system: &mut dyn CommandHandler = &mut self.file_system;
        let root: &mut dyn CommandHandler = &mut self.root;
        #[cfg(not(debug_assertions))]
        return vec![file_system, root];
        #[cfg(debug_assertions)]
        return vec![file_system, root, &mut self.debug];
    }

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ClosePrompt | Command::RenamePath(_, _) => {
                self.state.mode = InputMode::Normal;
                CommandResult::Handled
            }
            Command::OpenPrompt(_) => {
                self.state.mode = InputMode::Prompt;
                CommandResult::Handled
            }
            Command::SetFilter(filter) => {
                self.state.filter = filter.clone();
                self.state.mode = InputMode::Normal;
                CommandResult::Handled
            }
            Command::CopiedToClipboard(path) => {
                self.state.clipboard_command = Some(ClipboardCommand::Copy(path.clone()));
                CommandResult::Handled
            }
            Command::CutToClipboard(path) => {
                self.state.clipboard_command = Some(ClipboardCommand::Move(path.clone()));
                CommandResult::Handled
            }
            Command::ClearClipboard => {
                self.state.clipboard_command = None;
                CommandResult::Handled
            }
            Command::Resize(_, _) => CommandResult::Handled,
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('q'), KeyModifiers::NONE) => Command::Quit.into(),
            (_, _) => CommandResult::NotHandled,
        }
    }
}

fn recursively_handle_command(
    handler: &mut dyn CommandHandler,
    command: &Command,
    mode: &InputMode,
) -> (Vec<Command>, bool) {
    let mut derived_commands = Vec::new();

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
        derived_commands.push(*derived_command);
    }

    let child_derived_commands = handler.children().into_iter().flat_map(|child| {
        let (child_derived_commands, child_handled) =
            recursively_handle_command(child, command, mode);
        handled |= child_handled;
        child_derived_commands
    });

    derived_commands.extend(child_derived_commands);
    (derived_commands, handled)
}

fn must_not_contain_unhandled(commands: &[Command]) -> Result<()> {
    // Ignore unhandled Key or Mouse commands
    let unhandled_count = commands
        .iter()
        .filter(|command| {
            !matches!(command, Command::Key(_, _)) && !matches!(command, Command::Mouse(_))
        })
        .count();
    if unhandled_count > 0 {
        return Err(anyhow!(
            "Error: There are unhandled {unhandled_count} commands: {commands:?}"
        ));
    }
    Ok(())
}

fn should_quit(commands: &[Command]) -> bool {
    commands
        .iter()
        .any(|command| matches!(*command, Command::Quit))
}
