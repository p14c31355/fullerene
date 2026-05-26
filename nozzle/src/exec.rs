//! Command execution framework for Nozzle
//!
//! Defines the `Command` trait and the dispatch/listing functions.

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
/// Returns `true` to continue, `false` to exit.
pub fn dispatch(commands: &[&dyn Command], terminal: &mut dyn Terminal, line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }

    let args: alloc::vec::Vec<&str> = trimmed.split_whitespace().collect();
    if args.is_empty() {
        return true;
    }

    let cmd_name = args[0];

    // Built-in `help` command uses the dynamic command list.
    if cmd_name == "help" {
        list_commands(commands, terminal);
        return true;
    }

    for &cmd in commands {
        if cmd.name() == cmd_name {
            let mut ctx = CommandContext {
                terminal,
                args: &args,
            };
            return cmd.execute(&mut ctx);
        }
    }

    terminal.write_str("Unknown command: ");
    terminal.write_str(cmd_name);
    terminal.write_str("\nType 'help' for available commands.\n");
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
