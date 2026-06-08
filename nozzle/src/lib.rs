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
pub mod fs_hooks;
pub mod line_editor;
pub mod parser;
pub mod prompt;
pub mod selection;
pub mod sys_hooks;
pub mod terminal;
pub mod terminal_buffer;

use alloc::string::String;

pub use exec::{Command, CommandContext, NamedCommand};
pub use line_editor::LineEditor;
pub use parser::ParsedCommand;
pub use prompt::Prompt;
pub use terminal::Terminal;

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
            self.terminal
                .write_str("Nozzle shell — interactive OS runtime\n");
            self.terminal
                .write_str("Type 'help' for available commands.\n\n");
            self.welcome_shown = true;
        }
    }
}

/// Create the default command list (excluding `help`, which is handled by `dispatch`).
///
/// Returns a `&'static` slice suitable for passing to [`Shell::new`].
pub fn default_commands() -> &'static [&'static dyn Command] {
    use crate::builtins;
    define_commands!(
        ("clear", "Clear the screen", builtins::cmd_clear),
        ("echo", "Print text", builtins::cmd_echo),
        ("exit", "Exit the shell", builtins::cmd_exit),
        ("uname", "Show system information", builtins::cmd_uname),
        ("ls", "List files", builtins::cmd_ls),
        ("cat", "Print file contents", builtins::cmd_cat),
        ("pwd", "Print working directory", builtins::cmd_pwd),
        ("mem", "Show memory information", builtins::cmd_mem),
        ("tasks", "List processes", builtins::cmd_tasks),
        ("windows", "List windows", builtins::cmd_windows),
        ("dmesg", "Show kernel messages", builtins::cmd_dmesg),
        ("hexdump", "Hex dump of text", builtins::cmd_hexdump),
        ("version", "Show version info", builtins::cmd_version),
        ("reboot", "Reboot the system", builtins::cmd_reboot),
        ("shutdown", "Shutdown the system", builtins::cmd_shutdown),
        ("calc", "Simple arithmetic calculator", builtins::cmd_calc),
        ("run", "Launch an external application", builtins::cmd_run),
        ("taskmon", "Detailed task/process monitor", builtins::cmd_taskmon),
        ("devices", "List registered hardware devices", builtins::cmd_devices),
        ("theme", "Show or change desktop theme", builtins::cmd_theme),
        ("wallpaper", "Show or change desktop wallpaper", builtins::cmd_wallpaper),
        ("pci", "List PCI devices", builtins::cmd_pci),
        ("badapple", "Play Bad Apple!! animation", builtins::cmd_badapple),
    )
}
