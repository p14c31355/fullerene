use crate::pipeline::Pipeline;
use crate::terminal::Terminal;
use alloc::collections::VecDeque;
use spin::Mutex;

static SHARED_HISTORY: Mutex<VecDeque<alloc::string::String>> = Mutex::new(VecDeque::new());

pub fn get_history_snapshot() -> alloc::vec::Vec<alloc::string::String> {
    let guard = SHARED_HISTORY.lock();
    guard.iter().cloned().collect()
}

pub fn push_history(line: &str) {
    if line.is_empty() {
        return;
    }
    let mut guard = SHARED_HISTORY.lock();
    if guard.front().map_or(false, |h| h == line) {
        return;
    }
    if guard.len() >= 128 {
        guard.pop_back();
    }
    guard.push_front(alloc::string::String::from(line));
}

pub struct CommandContext<'a> {
    pub terminal: &'a mut dyn Terminal,
    pub args: &'a [&'a str],
}

pub trait Command {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn execute(&self, ctx: &mut CommandContext) -> bool;
}

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

/// Dispatch a command line, streaming the last stage's output directly
/// to the terminal without intermediate buffering.
///
/// For multi-stage pipelines (e.g. `ls | grep foo`), intermediate stages
/// still capture output into the pipe buffer (via `arm_pipe_stdout` /
/// `take_stdout`) so the next stage can consume it as input.  The *last*
/// stage writes directly through to the terminal, avoiding the cost of
/// buffering a potentially large result (e.g. `dmesg`) in a String only
/// to flush it in one shot.
pub fn dispatch(commands: &[&dyn Command], terminal: &mut dyn Terminal, line: &str) -> bool {
    terminal.write_str("dbg: dispatch entered\n");
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }

    let pipeline = Pipeline::parse(trimmed);
    terminal.write_str("dbg: pipeline parsed\n");
    if pipeline.commands.is_empty() {
        return true;
    }

    if pipeline.commands.len() == 1 && pipeline.commands[0].name == "help" {
        list_commands(commands, terminal);
        return true;
    }

    let mut pipe_buffer: Option<alloc::string::String> = None;

    for (i, cmd) in pipeline.commands.iter().enumerate() {
        terminal.write_str("dbg: iterating commands\n");
        let cmd_name = cmd.name.as_str();
        let is_last = i == pipeline.commands.len() - 1;

        let found = commands.iter().find(|c| c.name() == cmd_name);

        match found {
            Some(&matched) => {
                terminal.write_str("dbg: matched command\n");
                let mut args: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
                args.push(cmd_name);
                for a in &cmd.args {
                    args.push(a.as_str());
                }
                terminal.write_str("dbg: args built\n");

                if let Some(input) = pipe_buffer.take() {
                    terminal.set_stdin(input);
                }

                // Only buffer stdout for non-last stages.
                // The last stage streams directly to the terminal.
                if !is_last {
                    terminal.arm_pipe_stdout();
                }

                terminal.write_str("dbg: before execute\n");
                let mut ctx = CommandContext {
                    terminal,
                    args: &args,
                };
                let continue_shell = matched.execute(&mut ctx);

                if !is_last {
                    pipe_buffer = terminal.take_stdout();
                }
                // Always clear pipe stdin after each stage to prevent
                // stale data leaking into the next command line.
                terminal.clear_pipe_stdin();

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

    true
}

pub fn list_commands(commands: &[&dyn Command], terminal: &mut dyn Terminal) {
    terminal.write_str("Available commands:\n");
    for &cmd in commands {
        terminal.write_str("  ");
        terminal.write_str(cmd.name());
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

pub fn get_completions_for(prefix: &str, cmds: &[&dyn Command]) -> alloc::vec::Vec<alloc::string::String> {
    if prefix.contains(' ') {
        return alloc::vec::Vec::new();
    }
    let word = prefix.trim();
    let lower = word.to_lowercase();
    let mut matches = alloc::vec::Vec::new();
    for cmd in cmds.iter() {
        if cmd.name().starts_with(&lower) {
            matches.push(alloc::string::String::from(cmd.name()));
        }
    }
    matches
}
