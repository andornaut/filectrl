pub mod color;
pub mod command;
pub mod focus;

use self::command::{CommandHandler, CommandResult};
use self::focus::Focus;

use crate::{
    app::command::{
        Command, {receive_commands, spawn_command_sender},
    },
    components::root::Root,
    file_system::FileSystem,
    views::Renderable,
};
use anyhow::{anyhow, Result};
use crossterm::event::KeyCode;
use ratatui::{backend::Backend, Terminal};
use std::{sync::mpsc, thread, time::Duration};

const BROADCAST_CYCLES: u8 = 3;
const MAIN_LOOP_MIN_SLEEP_MS: u64 = 50;

#[derive(Default)]
pub struct App {
    file_system: FileSystem,
    focus: Focus,
    root: Root,
}

impl App {
    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        let (tx, rx) = mpsc::channel();

        // An initial command is required to start the main loop
        tx.send(self.file_system.cd_to_cwd()?)?;
        spawn_command_sender(tx);

        let min_duration = Duration::from_millis(MAIN_LOOP_MIN_SLEEP_MS);
        loop {
            let commands = receive_commands(&rx);

            if commands.is_empty() {
                // Only sleep if there are no commands, so that sequential inputs are processed ASAP
                thread::sleep(min_duration);
                continue;
            }

            let unhandled_commands = self.broadcast_commands(commands);
            if should_quit(&unhandled_commands) {
                return Ok(());
            }
            if !unhandled_commands.is_empty() {
                return Err(anyhow!(
                    "Error: There are unhandled commands: {unhandled_commands:?}"
                ));
            }
            self.render(terminal)?;
        }
    }

    fn broadcast_commands(&mut self, commands: Vec<Command>) -> Vec<Command> {
        let commands: Vec<Command> = commands
            .into_iter()
            .flat_map(|command| {
                self.handle_focus_command(&command);
                let command = self.convert_non_modal_key_command(command);
                self.broadcast_command(command)
            })
            .collect();
        commands
    }

    fn broadcast_command(&mut self, command: Command) -> Vec<Command> {
        let focus = &self.focus.clone();
        let mut commands: Vec<Command> = vec![command];
        for _ in 0..BROADCAST_CYCLES {
            commands = commands
                .into_iter()
                .flat_map(|command| {
                    let (mut derived_commands, handled) =
                        recursively_handle_command(focus, self, &command);
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

    fn convert_non_modal_key_command(&self, command: Command) -> Command {
        if self.focus == Focus::Modal {
            return command;
        }

        if let Command::Key(code, _) = command {
            return match code {
                KeyCode::Char('q') | KeyCode::Char('Q') => Command::Quit,
                KeyCode::Backspace => Command::BackDir,
                _ => command,
            };
        }

        command
    }

    fn handle_focus_command(&mut self, command: &Command) {
        match command {
            Command::NextFocus => self.focus.next(),
            Command::PreviousFocus => self.focus.previous(),
            _ => (),
        }
    }

    fn render<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        terminal.draw(|frame| {
            let window = frame.size();
            self.root.render(frame, window);
        })?;
        Ok(())
    }
}

impl CommandHandler for App {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        let file_system: &mut dyn CommandHandler = &mut self.file_system;
        let root: &mut dyn CommandHandler = &mut self.root;
        vec![file_system, root]
    }
}

fn recursively_handle_command(
    focus: &Focus,
    handler: &mut dyn CommandHandler,
    command: &Command,
) -> (Vec<Command>, bool) {
    let mut derived_commands = Vec::new();

    let result = if command.needs_focus() && !handler.is_focussed(focus) {
        CommandResult::NotHandled
    } else {
        handler.handle_command(command)
    };

    let mut handled = !matches!(result, CommandResult::NotHandled);

    if let CommandResult::Handled(Some(derived_command)) = result {
        derived_commands.push(derived_command);
    }

    let child_derived = handler.children().into_iter().flat_map(|child| {
        let (child_derived, child_handled) = recursively_handle_command(focus, child, command);
        handled = handled || child_handled;
        child_derived
    });

    derived_commands.extend(child_derived);
    (derived_commands, handled)
}

fn should_quit(commands: &[Command]) -> bool {
    commands
        .iter()
        .any(|command| matches!(*command, Command::Quit))
}
