pub mod config;
mod events;
pub mod terminal;

use std::{
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use log::debug;
use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseEvent},
    Frame,
};

use self::{
    config::Config,
    events::{receive_commands, spawn_command_sender},
    terminal::CleanupOnDropTerminal,
};
use crate::{
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
    file_system::FileSystem,
    views::{root::RootView, View},
};

const BROADCASTS_COUNT: u8 = 5;
const MAIN_LOOP_MAX_SLEEP: Duration = Duration::from_millis(30);

pub struct App {
    config: Config,
    file_system: FileSystem,
    mode: InputMode,
    root: RootView,
    terminal: CleanupOnDropTerminal,
}

impl App {
    pub fn new(config: Config, terminal: CleanupOnDropTerminal) -> Self {
        let file_system = FileSystem::new(&config);
        let root = RootView::new(&config);
        Self {
            config,
            file_system,
            mode: InputMode::default(),
            root,
            terminal,
        }
    }

    pub fn run(&mut self, initial_directory: Option<PathBuf>) -> Result<()> {
        let (tx, rx) = mpsc::channel();

        // An initial command is required to start the main loop
        tx.send(self.file_system.init(initial_directory, tx.clone())?)?;
        spawn_command_sender(tx);

        loop {
            let start = Instant::now();
            let commands = receive_commands(&rx);

            if commands.is_empty() {
                thread::sleep(MAIN_LOOP_MAX_SLEEP);
                continue;
            }

            let remaining_commands = self.broadcast_commands(commands);

            if should_quit(&remaining_commands) {
                return Ok(());
            }

            must_not_contain_unhandled(&remaining_commands)?;
            self.render()?;

            let actual_sleep =
                MAIN_LOOP_MAX_SLEEP.saturating_sub(Instant::now().duration_since(start));
            thread::sleep(actual_sleep);
        }
    }

    fn broadcast_commands(&mut self, commands: Vec<Command>) -> Vec<Command> {
        commands
            .into_iter()
            .flat_map(|command| self.broadcast_command(command))
            .collect()
    }

    fn broadcast_command(&mut self, command: Command) -> Vec<Command> {
        let mode = self.mode.clone();
        let mut commands: Vec<Command> = vec![command];
        for _ in 0..BROADCASTS_COUNT {
            commands = commands
                .into_iter()
                .flat_map(|command| {
                    // The same command may be broadcast up to BROADCAST_CYCLES times
                    // if it doesn't produce a derived command. This seems wasteful,
                    // but it only occurs for Command::Quit or if the command is
                    // ultimately unhandled, which results in an error anyway.
                    //debug!("broadcast_command() {command:?}");
                    let (mut derived_commands, handled) =
                        recursively_handle_command(self, &command, &mode);
                    if !handled {
                        derived_commands.push(command);
                    }
                    derived_commands
                })
                .collect();
            if commands.is_empty() {
                break;
            }
        }
        commands
    }

    fn render(&mut self) -> Result<()> {
        self.terminal.draw(|frame: &mut Frame| {
            let area = frame.area();
            self.root
                .render(area, frame, &self.mode, &self.config.theme);
        })?;
        Ok(())
    }

    fn set_mode(&mut self, mode: InputMode) -> CommandResult {
        debug!("Setting mode to: {:?}", mode);
        self.mode = mode;
        CommandResult::Handled
    }
}

impl CommandHandler for App {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        let file_system: &mut dyn CommandHandler = &mut self.file_system;
        let root: &mut dyn CommandHandler = &mut self.root;
        vec![file_system, root]
    }

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ClosePrompt => self.set_mode(InputMode::Normal),
            Command::OpenPrompt(_) => self.set_mode(InputMode::Prompt),
            Command::RenamePath(_, _) => self.set_mode(InputMode::Normal),
            Command::Resize(_, _) => CommandResult::Handled, // TODO can probably remove Command::Resize
            Command::SetFilter(_) => self.set_mode(InputMode::Normal),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('q'), KeyModifiers::NONE) => Command::Quit.into(),
            // TODO remove
            (KeyCode::Char('1'), KeyModifiers::NONE) => {
                Command::AlertInfo("Test info alert".into()).into()
            }
            (KeyCode::Char('2'), KeyModifiers::NONE) => {
                Command::AlertWarn("Test warning alert".into()).into()
            }
            (KeyCode::Char('3'), KeyModifiers::NONE) => {
                Command::AlertError("Test error alert".into()).into()
            }
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
            if handler.should_receive_key(mode) {
                handler.handle_key(code, modifiers)
            } else {
                CommandResult::NotHandled
            }
        }
        Command::Mouse(mouse_event) => {
            let MouseEvent { column, row, .. } = mouse_event;
            if handler.should_receive_mouse(*column, *row) {
                handler.handle_mouse(mouse_event)
            } else {
                CommandResult::NotHandled
            }
        }
        _ => handler.handle_command(command),
    };

    let mut handled = !matches!(result, CommandResult::NotHandled);

    if let CommandResult::HandledWith(derived_command) = result {
        derived_commands.push(derived_command);
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
