use crate::pipeline::Pipeline;
use crate::terminal::Terminal;
use core::any::Any;

pub struct CommandContext<'a> {
    pub terminal: &'a mut dyn Terminal,
    pub args: &'a [&'a str],
    services: Option<&'a dyn Any>,
}

impl CommandContext<'_> {
    /// Retrieve constructor-injected command services by their concrete type.
    pub fn services<T: Any + Copy>(&self) -> Option<T> {
        self.services?.downcast_ref().copied()
    }
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
    dispatch_inner(commands, terminal, line, None)
}

/// Dispatch a command line with immutable, constructor-injected services.
pub fn dispatch_with_services(
    commands: &[&dyn Command],
    terminal: &mut dyn Terminal,
    line: &str,
    services: &dyn Any,
) -> bool {
    dispatch_inner(commands, terminal, line, Some(services))
}

fn dispatch_inner(
    commands: &[&dyn Command],
    terminal: &mut dyn Terminal,
    line: &str,
    services: Option<&dyn Any>,
) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }

    let pipeline = Pipeline::parse(trimmed);
    if pipeline.commands.is_empty() {
        return true;
    }

    if pipeline.commands.len() == 1 && pipeline.commands[0].name == "help" {
        list_commands(commands, terminal);
        return true;
    }

    let mut pipe_buffer: Option<alloc::string::String> = None;

    for (i, cmd) in pipeline.commands.iter().enumerate() {
        let cmd_name = cmd.name.as_str();
        let is_last = i == pipeline.commands.len() - 1;

        let found = commands.iter().find(|c| c.name() == cmd_name);

        match found {
            Some(&matched) => {
                let mut args: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
                args.push(cmd_name);
                for a in &cmd.args {
                    args.push(a.as_str());
                }

                if let Some(input) = pipe_buffer.take() {
                    terminal.set_stdin(input);
                }

                // Only buffer stdout for non-last stages.
                // The last stage streams directly to the terminal.
                if !is_last {
                    terminal.arm_pipe_stdout();
                }

                let mut ctx = CommandContext {
                    terminal,
                    args: &args,
                    services,
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

pub fn get_completions_for(
    prefix: &str,
    cmds: &[&dyn Command],
) -> alloc::vec::Vec<alloc::string::String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[derive(Default)]
    struct FakeTerminal {
        output: String,
        pipe_stdout: Option<String>,
        pipe_stdin: Option<String>,
        capture: bool,
    }

    impl Terminal for FakeTerminal {
        fn write_str(&mut self, value: &str) {
            if self.capture {
                self.pipe_stdout
                    .get_or_insert_with(String::new)
                    .push_str(value);
            } else {
                self.output.push_str(value);
            }
        }

        fn read_byte(&mut self) -> Option<u8> {
            None
        }

        fn set_stdin(&mut self, data: String) {
            self.pipe_stdin = Some(data);
        }

        fn take_stdout(&mut self) -> Option<String> {
            self.capture = false;
            self.pipe_stdout.take()
        }

        fn take_stdin(&mut self) -> Option<String> {
            self.pipe_stdin.take()
        }

        fn arm_pipe_stdout(&mut self) {
            self.capture = true;
            self.pipe_stdout = Some(String::new());
        }

        fn clear_pipe_stdin(&mut self) {
            self.pipe_stdin = None;
        }
    }

    fn emit(ctx: &mut CommandContext) -> bool {
        ctx.terminal.write_str("pipeline data");
        true
    }

    fn consume(ctx: &mut CommandContext) -> bool {
        let input = ctx.terminal.take_stdin().unwrap_or_default();
        ctx.terminal.write_str(&input);
        true
    }

    fn stop(_ctx: &mut CommandContext) -> bool {
        false
    }

    static EMIT: NamedCommand = NamedCommand {
        name: "emit",
        description: "emit",
        func: emit,
    };
    static CONSUME: NamedCommand = NamedCommand {
        name: "consume",
        description: "consume",
        func: consume,
    };
    static STOP: NamedCommand = NamedCommand {
        name: "stop",
        description: "stop",
        func: stop,
    };

    #[test]
    fn unknown_command_reports_error() {
        let mut terminal = FakeTerminal::default();
        assert!(dispatch(&[], &mut terminal, "missing"));
        assert!(terminal.output.contains("Unknown command: missing"));
    }

    #[test]
    fn pipeline_routes_stdout_to_next_stdin() {
        let commands: &[&dyn Command] = &[&EMIT, &CONSUME];
        let mut terminal = FakeTerminal::default();
        assert!(dispatch(commands, &mut terminal, "emit | consume"));
        assert_eq!(terminal.output, "pipeline data");
        assert!(terminal.pipe_stdin.is_none());
    }

    #[test]
    fn command_can_stop_dispatch() {
        let commands: &[&dyn Command] = &[&STOP];
        let mut terminal = FakeTerminal::default();
        assert!(!dispatch(commands, &mut terminal, "stop"));
    }

    #[test]
    fn dispatch_with_services_injects_service() {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        struct MyService(u32);

        fn check_service(ctx: &mut CommandContext) -> bool {
            let value = ctx.services::<MyService>().map(|s| s.0).unwrap_or(0);
            ctx.terminal.write_str(&alloc::format!("service={}", value));
            true
        }

        static CHECK: NamedCommand = NamedCommand {
            name: "check",
            description: "check",
            func: check_service,
        };

        let service = MyService(42);
        let mut terminal = FakeTerminal::default();
        let commands: &[&dyn Command] = &[&CHECK];
        assert!(dispatch_with_services(
            &commands,
            &mut terminal,
            "check",
            &service
        ));
        assert!(terminal.output.contains("service=42"));
    }

    #[test]
    fn dispatch_without_services_returns_none() {
        fn check_service(ctx: &mut CommandContext) -> bool {
            let has = ctx.services::<u32>().is_some();
            ctx.terminal
                .write_str(&alloc::format!("has_service={}", has));
            true
        }

        static CHECK: NamedCommand = NamedCommand {
            name: "check",
            description: "check",
            func: check_service,
        };

        let mut terminal = FakeTerminal::default();
        let commands: &[&dyn Command] = &[&CHECK];
        assert!(dispatch(commands, &mut terminal, "check"));
        assert!(terminal.output.contains("has_service=false"));
    }
}
