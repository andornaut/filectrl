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
use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const BROADCAST_CYCLES: u8 = 4;
const MAIN_LOOP_MAX_SLEEP_MS: u64 = 30;

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
        tx.send(self.file_system.cd_to_cwd())?;
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
            self.render(terminal)?;

            let actual_sleep = max_sleep.saturating_sub(Instant::now().duration_since(start));
            thread::sleep(actual_sleep);
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

    fn render<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        terminal.draw(|frame| {
            let window = frame.size();
            self.root.render(frame, window, &self.focus);
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

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::NextFocus => self.focus.next(),
            Command::PreviousFocus => self.focus.previous(),
            Command::Focus(focus) => {
                self.focus = focus.clone();
            }
            Command::Resize(w, h) => {
                eprintln!("TODO App.handle_command() command:{command:?}");
            }
            _ => return CommandResult::NotHandled,
        }
        CommandResult::none()
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
