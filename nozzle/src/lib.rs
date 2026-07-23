#![no_std]
extern crate alloc;

pub mod builtins;
pub mod fs_hooks;
pub mod line_editor;
pub mod prompt;
pub mod selection;
pub mod sys_hooks;
pub mod terminal_buffer;

use alloc::string::String;

// Re-export carrier types so existing consumers still work
pub use carrier::exec::{Command, CommandContext, NamedCommand};
pub use carrier::pipeline::ParsedCommand;
pub use carrier::terminal::Terminal;

pub use line_editor::LineEditor;
pub use prompt::Prompt;

pub const DEFAULT_PROMPT: &str = "nozzle> ";

/// Immutable services required by Nozzle built-ins.
#[derive(Clone, Copy)]
pub struct ShellServices {
    pub fs: fs_hooks::FsHooks,
    pub sys: sys_hooks::SysHooks,
    pub mount: Option<fn(&mut CommandContext)>,
}

impl ShellServices {
    pub const fn new(
        fs: fs_hooks::FsHooks,
        sys: sys_hooks::SysHooks,
        mount: Option<fn(&mut CommandContext)>,
    ) -> Self {
        Self { fs, sys, mount }
    }

    pub const fn none() -> Self {
        Self::new(fs_hooks::FsHooks::none(), sys_hooks::SysHooks::none(), None)
    }
}

pub(crate) fn services(ctx: &CommandContext) -> Option<ShellServices> {
    ctx.services::<ShellServices>()
}

pub struct Shell<'a> {
    terminal: &'a mut dyn Terminal,
    commands: &'a [&'a dyn Command],
    editor: LineEditor,
    prompt: Prompt,
    welcome_shown: bool,
    services: ShellServices,
}

impl<'a> Shell<'a> {
    pub fn new(
        terminal: &'a mut dyn Terminal,
        commands: &'a [&'a dyn Command],
        services: ShellServices,
    ) -> Self {
        Self {
            terminal,
            commands,
            editor: LineEditor::new(),
            prompt: Prompt::new(DEFAULT_PROMPT),
            welcome_shown: false,
            services,
        }
    }

    pub fn set_prompt(&mut self, text: impl Into<String>) {
        self.prompt.set_text(text);
    }

    pub fn run(&mut self) {
        self.run_with_initial_line(None);
    }

    pub fn run_with_initial_line(&mut self, initial_line: Option<&str>) {
        self.show_welcome();

        if let Some(line) = initial_line {
            self.terminal.write_str(self.prompt.as_str());
            self.terminal.write_str(line);
            self.terminal.write_str("\n");
            if !self.execute_line(line) {
                self.terminal.write_str("Shell exited.\n");
            }

            // An initial line is a deferred, one-shot launch (for example a
            // file opened from the desktop).  Do not immediately enter the
            // blocking interactive read loop here: the caller is running on
            // the scheduler stack and must return so the compositor can
            // render the command's output.  A later explicit shell launch
            // starts the interactive session normally.
            return;
        }

        loop {
            self.terminal.write_str(self.prompt.as_str());

            let line = match self.editor.read_line(&mut *self.terminal) {
                Some(l) => l,
                None => {
                    continue;
                }
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if !self.execute_line(trimmed) {
                break;
            }
        }

        self.terminal.write_str("Shell exited.\n");
    }

    pub fn execute_line(&mut self, line: &str) -> bool {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return true;
        }
        carrier::exec::dispatch_with_services(
            self.commands,
            &mut *self.terminal,
            trimmed,
            &self.services,
        )
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

pub fn default_commands() -> &'static [&'static dyn Command] {
    use crate::builtins;
    carrier::define_commands!(
        ("clear", "Clear the screen", builtins::cmd_clear),
        ("echo", "Print text", builtins::cmd_echo),
        ("exit", "Exit the shell", builtins::cmd_exit),
        ("uname", "Show system information", builtins::cmd_uname),
        ("ls", "List files", builtins::cmd_ls),
        ("cat", "Print file contents", builtins::cmd_cat),
        ("pwd", "Print working directory", builtins::cmd_pwd),
        ("mem", "Show memory information", builtins::cmd_mem),
        (
            "metrics",
            "Show boot/frame/heap/DMA metrics",
            builtins::cmd_metrics
        ),
        (
            "cpuinfo",
            "Show discovered processor topology",
            builtins::cmd_cpuinfo
        ),
        ("tasks", "List processes", builtins::cmd_tasks),
        ("windows", "List windows", builtins::cmd_windows),
        ("dmesg", "Show kernel messages", builtins::cmd_dmesg),
        ("hexdump", "Hex dump of text", builtins::cmd_hexdump),
        ("version", "Show version info", builtins::cmd_version),
        ("reboot", "Reboot the system", builtins::cmd_reboot),
        ("shutdown", "Shutdown the system", builtins::cmd_shutdown),
        ("calc", "Simple arithmetic calculator", builtins::cmd_calc),
        ("run", "Launch an external application", builtins::cmd_run),
        (
            "taskmon",
            "Detailed task/process monitor",
            builtins::cmd_taskmon
        ),
        (
            "devices",
            "List registered hardware devices",
            builtins::cmd_devices
        ),
        ("theme", "Show or change desktop theme", builtins::cmd_theme),
        (
            "wallpaper",
            "Show or change desktop wallpaper",
            builtins::cmd_wallpaper
        ),
        ("pci", "List PCI devices", builtins::cmd_pci),
        (
            "badapple",
            "Play Bad Apple!! animation",
            builtins::cmd_badapple
        ),
        ("cd", "Change working directory", builtins::cmd_cd),
        ("tree", "Display directory tree", builtins::cmd_tree),
        ("find", "Search for files", builtins::cmd_find),
        ("cp", "Copy a file", builtins::cmd_cp),
        ("mv", "Move a file", builtins::cmd_mv),
        ("write", "Write content to a file", builtins::cmd_write),
        (
            "app",
            "Package manager (install/remove/list)",
            builtins::cmd_app
        ),
        ("rm", "Remove files or directories", builtins::cmd_rm),
        ("mkdir", "Create directories", builtins::cmd_mkdir),
        ("touch", "Create empty files", builtins::cmd_touch),
        ("df", "Show disk usage", builtins::cmd_df),
        ("date", "Show current date and time", builtins::cmd_date),
        ("uptime", "Show system uptime", builtins::cmd_uptime),
        ("whoami", "Print current user name", builtins::cmd_whoami),
        ("history", "Show command history", builtins::cmd_history),
        ("sleep", "Pause for N seconds", builtins::cmd_sleep),
        ("grep", "Search for a pattern", builtins::cmd_grep),
        ("sort", "Sort lines of text", builtins::cmd_sort),
        ("wc", "Count lines, words, and bytes", builtins::cmd_wc),
        (
            "mount",
            "Mount a block device to a directory",
            builtins::cmd_mount
        ),
        ("usb_info", "Show USB device status", builtins::cmd_usb_info),
        (
            "usb_rescan",
            "Explicitly activate and rescan USB",
            builtins::cmd_usb_rescan
        ),
        (
            "sd_rescan",
            "Rescan the SD card reader without mounting",
            builtins::cmd_sd_rescan
        ),
        (
            "hello_linux",
            "Launch the built-in Linux test binary",
            builtins::cmd_hello_linux
        ),
        (
            "linux_run",
            "Launch a Linux ELF binary from the filesystem",
            builtins::cmd_linux_run
        ),
        (
            "run_busybox",
            "Launch BusyBox shell from the filesystem",
            builtins::cmd_run_busybox
        ),
        ("wasm", "Run a WASM/WASI binary", builtins::cmd_wasm),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    struct OneShotTerminal {
        output: String,
    }

    impl Terminal for OneShotTerminal {
        fn write_str(&mut self, s: &str) {
            self.output.push_str(s);
        }

        fn read_byte(&mut self) -> Option<u8> {
            panic!("one-shot shell unexpectedly entered interactive input");
        }
    }

    #[test]
    fn initial_command_returns_without_entering_interactive_loop() {
        let mut terminal = OneShotTerminal {
            output: String::new(),
        };
        let mut shell = Shell::new(&mut terminal, default_commands(), ShellServices::none());

        shell.run_with_initial_line(Some("echo hello"));

        assert!(terminal.output.contains("echo hello\n"));
        assert!(terminal.output.contains("hello\n"));
    }
}

pub fn get_completions(prefix: &str) -> alloc::vec::Vec<alloc::string::String> {
    carrier::exec::get_completions_for(prefix, default_commands())
}
