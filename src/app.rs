pub mod color;
mod events;
pub mod focus;

use self::{
    events::{receive_commands, spawn_command_sender},
    focus::Focus,
};
use crate::{
    command::{handler::CommandHandler, result::CommandResult, Command},
    file_system::FileSystem,
    views::{root::RootView, View},
};
use anyhow::{anyhow, Result};
use ratatui::{backend::Backend, Terminal};
use std::{sync::mpsc, thread, time::Duration};

const BROADCAST_CYCLES: u8 = 3;
const MAIN_LOOP_MIN_SLEEP_MS: u64 = 50;

#[derive(Default)]
pub struct App {
    file_system: FileSystem,
    focus: Focus,
    root: RootView,
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

            let remaining_commands = self.broadcast_commands(commands);

            if should_quit(&remaining_commands) {
                return Ok(());
            }

            must_not_contain_unhandled(&remaining_commands)?;
            self.render(terminal)?;
        }
    }

    fn broadcast_commands(&mut self, commands: Vec<Command>) -> Vec<Command> {
        let commands: Vec<Command> = commands
            .into_iter()
            .flat_map(|command| self.broadcast_command(command))
            .collect();
        commands
    }

    fn broadcast_command(&mut self, command: Command) -> Vec<Command> {
        let command = self.translate_non_prompt_key_command(command);
        let focus = self.focus.clone();
        eprintln!("broadcast_command: focus:{focus:?} command:{command:?}");

        let mut commands: Vec<Command> = vec![command];
        for _ in 0..BROADCAST_CYCLES {
            commands = commands
                .into_iter()
                .flat_map(|command| {
                    if self.handle_focus_command(&command) {
                        // Shortcut: Focus Commands are not handled by Views/Handlers.
                        return Vec::new();
                    }

                    let (mut derived_commands, handled) =
                        recursively_handle_command(&focus, self, &command);
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

    fn handle_focus_command(&mut self, command: &Command) -> bool {
        match command {
            Command::NextFocus => self.focus.next(),
            Command::PreviousFocus => self.focus.previous(),
            Command::Focus(focus) => {
                eprintln!("Changed focus to: {focus:?}");
                self.focus = focus.clone();
            }
            _ => return false,
        }
        true
    }

    fn render<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        terminal.draw(|frame| {
            let window = frame.size();
            self.root.render(frame, window);
        })?;
        Ok(())
    }

    fn translate_non_prompt_key_command(&self, command: Command) -> Command {
        if self.focus == Focus::Prompt {
            return command;
        }
        command.translate_non_prompt_key_command()
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
