//! Command execution framework for Nozzle
//!
//! Defines the `Command` trait and the dispatch/listing functions.

use crate::parser::Pipeline;
use crate::terminal::Terminal;

/// Context provided to every command execution
pub struct CommandContext<'a> {
    /// Terminal for I/O
    pub terminal: &'a mut dyn Terminal,
    /// Raw arguments (args[0] is the command name)
    pub args: &'a [&'a str],
}

/// Trait for a single shell command.
// Dyn-compatible: no generics on `execute`.
pub trait Command {
    /// Display name (used for matching against the first token).
    fn name(&self) -> &'static str;

    /// One-line description shown in `help`.
    fn description(&self) -> &'static str;

    /// Execute the command.
    /// Return `true` to continue the shell loop, `false` to exit.
    fn execute(&self, ctx: &mut CommandContext) -> bool;
}

/// A concrete command backed by a static function pointer.
pub struct NamedCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub func: fn(&mut CommandContext) -> bool,
}

impl Command for NamedCommand {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn execute(&self, ctx: &mut CommandContext) -> bool {
        (self.func)(ctx)
    }
}

/// Build a `&[&dyn Command]` from named entries.
#[macro_export]
macro_rules! define_commands {
    ($(($name:expr, $desc:expr, $func:path)),* $(,)?) => {
        &[
            $(
                &$crate::exec::NamedCommand {
                    name: $name,
                    description: $desc,
                    func: $func,
                } as &dyn $crate::exec::Command
            ),*
        ] as &[&dyn $crate::exec::Command]
    };
}

/// Dispatch a command line against a command list.
///
/// Supports pipe (`|`) chaining: outputs from earlier commands are
/// collected and fed as input to the next command via `set_stdin`.
/// Returns `true` to continue, `false` to exit.
pub fn dispatch(commands: &[&dyn Command], terminal: &mut dyn Terminal, line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }

    let pipeline = Pipeline::parse(trimmed);
    if pipeline.commands.is_empty() {
        return true;
    }

    // Built-in `help` command uses the dynamic command list.
    if pipeline.commands.len() == 1 && pipeline.commands[0].name == "help" {
        list_commands(commands, terminal);
        return true;
    }

    // Execute commands in the pipeline, feeding stdout of one
    // as stdin of the next.
    let mut pipe_buffer: Option<alloc::string::String> = None;

    for cmd in &pipeline.commands {
        let cmd_name = cmd.name.as_str();

        let found = commands.iter().find(|c| c.name() == cmd_name);

        match found {
            Some(&matched) => {
                // Build argument list (name + args).
                let mut args: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
                args.push(cmd_name);
                for a in &cmd.args {
                    args.push(a.as_str());
                }

                // Set stdin if we have piped data from the previous command.
                if let Some(ref input) = pipe_buffer.take() {
                    terminal.set_stdin(alloc::string::String::from(input.as_str()));
                }

                let mut ctx = CommandContext {
                    terminal,
                    args: &args,
                };
                let continue_shell = matched.execute(&mut ctx);

                // Collect stdout for the next pipe stage.
                pipe_buffer = terminal.take_stdout();

                if !continue_shell {
                    return false;
                }
            }
            None => {
                terminal.write_str("Unknown command: ");
                terminal.write_str(cmd_name);
                terminal.write_str("\nType 'help' for available commands.\n");
                return true;
            }
        }
    }

    // Flush the final output to the terminal.
    if let Some(output) = pipe_buffer {
        if !output.is_empty() {
            terminal.write_str(&output);
            terminal.write_str("\n");
        }
    }

    true
}

/// List all command names and descriptions.
pub fn list_commands(commands: &[&dyn Command], terminal: &mut dyn Terminal) {
    terminal.write_str("Available commands:\n");
    for &cmd in commands {
        terminal.write_str("  ");
        terminal.write_str(cmd.name());
        // Pad to 12 columns
        let pad = if cmd.name().len() < 12 {
            12 - cmd.name().len()
        } else {
            1
        };
        for _ in 0..pad {
            terminal.write_str(" ");
        }
        terminal.write_str("- ");
        terminal.write_str(cmd.description());
        terminal.write_str("\n");
    }
}

/// Get TAB completion candidates for a partial command line.
pub fn get_completions(prefix: &str) -> alloc::vec::Vec<alloc::string::String> {
    let word = prefix.trim().split_whitespace().next().unwrap_or("");
    let lower = word.to_lowercase();
    let mut matches = alloc::vec::Vec::new();
    let cmds = crate::default_commands();
    for cmd in cmds.iter() {
        if cmd.name().starts_with(&lower) {
            matches.push(alloc::string::String::from(cmd.name()));
        }
    }
    matches
}
