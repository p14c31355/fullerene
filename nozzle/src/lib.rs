//! Nozzle — interactive shell runtime for Fullerene OS
//!
//! Nozzle is a no_std interactive shell runtime that provides:
//!
//! - A [`Terminal`] trait for abstract I/O
//! - A [`LineEditor`] with history, cursor movement, and editing
//! - A [`Command`] trait for extensible built-in commands
//! - Built-in commands: `clear`, `echo`, `exit`, `uname`
//! - `help` is handled by the dispatch function directly
//! - A main loop that ties everything together
//!
//! Nozzle knows nothing about framebuffers, graphics, or the kernel
//! — it only needs a text-based I/O stream.

#![no_std]
extern crate alloc;

pub mod builtins;
pub mod exec;
pub mod line_editor;
pub mod parser;
pub mod prompt;
pub mod terminal;

use alloc::string::String;
use alloc::vec::Vec;

pub use exec::{Command, CommandContext, NamedCommand};
pub use terminal::Terminal;
pub use line_editor::LineEditor;
pub use parser::ParsedCommand;
pub use prompt::Prompt;

/// Default shell prompt
pub const DEFAULT_PROMPT: &str = "nozzle> ";

/// The Nozzle shell runtime.
///
/// Wires together a [`Terminal`], [`LineEditor`], and command list
/// into an interactive read-eval-print loop.
pub struct Shell<'a> {
    terminal: &'a mut dyn Terminal,
    commands: &'a [&'a dyn Command],
    editor: LineEditor,
    prompt: Prompt,
    welcome_shown: bool,
}

impl<'a> Shell<'a> {
    /// Create a new shell instance.
    pub fn new(terminal: &'a mut dyn Terminal, commands: &'a [&'a dyn Command]) -> Self {
        Self {
            terminal,
            commands,
            editor: LineEditor::new(),
            prompt: Prompt::new(DEFAULT_PROMPT),
            welcome_shown: false,
        }
    }

    /// Set the prompt string.
    pub fn set_prompt(&mut self, text: impl Into<String>) {
        self.prompt.set_text(text);
    }

    /// Run the main shell loop.
    ///
    /// This function returns when the `exit` command is executed.
    pub fn run(&mut self) {
        self.show_welcome();

        loop {
            // Show prompt
            self.terminal.write_str(self.prompt.as_str());

            // Read a line
            let line = match self.editor.read_line(&mut *self.terminal) {
                Some(l) => l,
                None => {
                    // Ctrl+C or Ctrl+D on empty line
                    continue;
                }
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Dispatch and execute
            let should_continue = exec::dispatch(self.commands, &mut *self.terminal, trimmed);
            if !should_continue {
                break;
            }
        }

        self.terminal.write_str("Shell exited.\n");
    }

    fn show_welcome(&mut self) {
        if !self.welcome_shown {
            self.terminal.write_str("Nozzle shell — interactive OS runtime\n");
            self.terminal.write_str("Type 'help' for available commands.\n\n");
            self.welcome_shown = true;
        }
    }
}

/// Convenience function: build and run a shell with default builtins.
///
/// `extra_commands` must be a static slice (use `&define_commands!(...)`).
pub fn run_shell(
    terminal: &mut dyn Terminal,
    extra_commands: &'static [&'static dyn Command],
) {
    merge_and_run(terminal, default_commands(), extra_commands)
}

/// Internal helper: merge two static command slices, leak, and run.
fn merge_and_run(
    terminal: &mut dyn Terminal,
    defaults: &'static [&'static dyn Command],
    extra: &'static [&'static dyn Command],
) {
    let mut all: Vec<&'static dyn Command> = Vec::new();
    all.extend_from_slice(defaults);
    all.extend_from_slice(extra);
    let leaked: &'static [&'static dyn Command] = alloc::boxed::Box::leak(all.into_boxed_slice());
    let mut shell = Shell::new(terminal, leaked);
    shell.run();
}

/// Create the default command list (excluding `help`, which is handled by `dispatch`).
pub fn default_commands() -> &'static [&'static dyn Command] {
    use crate::builtins;
    define_commands!(
        ("clear", "Clear the screen",        builtins::cmd_clear),
        ("echo",  "Print text",              builtins::cmd_echo),
        ("exit",  "Exit the shell",          builtins::cmd_exit),
        ("uname", "Show system information", builtins::cmd_uname),
    )
}