pub mod config;
mod default_config;
mod events;
pub mod terminal;
pub mod theme;

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
use anyhow::{anyhow, Result};
use copypasta::{ClipboardContext, ClipboardProvider};
use crossterm::event::{KeyCode, KeyModifiers, MouseEvent};
use std::{
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const BROADCAST_CYCLES: u8 = 5;
const MAIN_LOOP_MAX_SLEEP_MS: u64 = 30;

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
        Self {
            config,
            file_system,
            mode: InputMode::default(),
            root: RootView::default(),
            terminal,
        }
    }

    pub fn run(&mut self, directory: Option<PathBuf>) -> Result<()> {
        let (tx, rx) = mpsc::channel();

        // An initial command is required to start the main loop
        tx.send(self.file_system.init(directory)?)?;
        spawn_command_sender(tx);

        let max_sleep = Duration::from_millis(MAIN_LOOP_MAX_SLEEP_MS);
        loop {
            let start = Instant::now();
            let commands = receive_commands(&rx);

            if commands.is_empty() {
                thread::sleep(max_sleep);
                continue;
            }

            let remaining_commands = self.broadcast_commands(commands);

            if should_quit(&remaining_commands) {
                return Ok(());
            }

            must_not_contain_unhandled(&remaining_commands)?;
            self.render()?;

            let actual_sleep = max_sleep.saturating_sub(Instant::now().duration_since(start));
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
        for _ in 0..BROADCAST_CYCLES {
            commands = commands
                .into_iter()
                .flat_map(|command| {
                    // The same command may be broadcast up to BROADCAST_CYCLES times
                    // if it doesn't produce a derived command. This seems wasteful,
                    // but it only occurs for Command::Quit or if the command is
                    // ultimately unhandled, which results in an error anyway.
                    eprintln!("App.broadcast_command() command:{command:?}");
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
        self.terminal.draw(|frame| {
            let window = frame.size();
            self.root
                .render(frame, window, &self.mode, &self.config.theme);
        })?;
        Ok(())
    }

    fn set_mode(&mut self, mode: InputMode) -> CommandResult {
        self.mode = mode;
        CommandResult::none()
    }

    fn clipboard_copy(&mut self) -> CommandResult {
        let mut ctx = ClipboardContext::new().unwrap();
        let content = ctx.get_contents().unwrap();
        eprintln!("TODO App.clipboard_copy(): {content}");
        CommandResult::none()
    }

    fn clipboard_cut(&mut self) -> CommandResult {
        let mut ctx = ClipboardContext::new().unwrap();
        let content = ctx.get_contents().unwrap();
        eprintln!("TODO App.clipboard_cut(): {content}");
        CommandResult::none()
    }

    fn clipboard_paste(&mut self) -> CommandResult {
        let mut ctx = ClipboardContext::new().unwrap();
        let content = ctx.get_contents().unwrap();
        eprintln!("TODO App.clipboard_paste(): {content}");
        CommandResult::none()
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
            Command::Resize(_, _) => CommandResult::none(), // TODO can probably remove Command::Resize
            Command::SetFilter(_) => self.set_mode(InputMode::Normal),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('q'), _) => Command::Quit.into(),
            (KeyCode::Char('c'), KeyModifiers::CONTROL)
            | (KeyCode::Char('c'), KeyModifiers::SUPER) => self.clipboard_copy(),
            (KeyCode::Char('x'), KeyModifiers::CONTROL)
            | (KeyCode::Char('x'), KeyModifiers::SUPER) => self.clipboard_cut(),
            (KeyCode::Char('v'), KeyModifiers::CONTROL)
            | (KeyCode::Char('v'), KeyModifiers::SUPER) => self.clipboard_paste(),
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

    if let CommandResult::Handled(Some(derived_command)) = result {
        derived_commands.push(derived_command);
    }

    let child_derived = handler.children().into_iter().flat_map(|child| {
        let (child_derived, child_handled) = recursively_handle_command(child, command, mode);
        handled |= child_handled;
        child_derived
    });

    derived_commands.extend(child_derived);
    (derived_commands, handled)
}

fn must_not_contain_unhandled(commands: &[Command]) -> Result<()> {
    let unhandled_count = commands
        .into_iter()
        .filter(|command| !matches!(command, Command::Key(_, _)))
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
