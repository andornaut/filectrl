pub mod color;
pub mod command;

use crate::{
    app::command::{
        Command, {receive_commands, spawn_command_sender},
    },
    component::root::Root,
    file_system::FileSystem,
    view::Renderable,
};
use anyhow::{anyhow, Result};
use ratatui::{backend::Backend, Terminal};
use std::{sync::mpsc, thread, time::Duration};

use self::command::CommandHandler;

const BROADCAST_CYCLES: u8 = 3;
const MAIN_LOOP_MIN_SLEEP_MS: u64 = 30;

#[derive(Default)]
pub struct App {
    file_system: FileSystem,
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
            for command in commands.iter() {
                if let Command::Quit = command {
                    return Ok(());
                }
            }

            if !commands.is_empty() {
                self.broadcast_commands(commands)?;
                self.render(terminal)?;
            }
            thread::sleep(min_duration);
        }
    }

    fn broadcast_commands(&mut self, commands: Vec<Command>) -> Result<()> {
        let commands: Vec<Command> = commands
            .into_iter()
            .flat_map(|command| self.broadcast_command(command))
            .collect();
        must_not_have_unhandled_commands(&commands)
    }

    fn broadcast_command(&mut self, command: Command) -> Vec<Command> {
        let mut commands: Vec<Command> = vec![command];
        for _ in 0..BROADCAST_CYCLES {
            commands = commands
                .iter()
                .flat_map(|command| recursively_handle_command(self, command))
                .filter(|command| command.is_actionable())
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

fn must_not_have_unhandled_commands(commands: &[Command]) -> Result<()> {
    if commands.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "Error: There are unhandled commands: {:?}",
            commands
        ))
    }
}

fn recursively_handle_command(handler: &mut dyn CommandHandler, command: &Command) -> Vec<Command> {
    let mut commands = Vec::new();
    if let Some(c) = handler.handle_command(command) {
        commands.push(c);
    }

    let child_commands = handler
        .children()
        .into_iter()
        .flat_map(|child| recursively_handle_command(child, command));
    commands.extend(child_commands);
    commands
}
