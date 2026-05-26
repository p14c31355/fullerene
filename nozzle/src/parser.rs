//! Simple command-line parser for Nozzle
//!
//! Splits input into command name and arguments.
//! Supports pipe (`|`) chaining and basic quoting.

use alloc::fmt;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

/// A pipeline of commands connected by `|`.
///
/// Example: `ls | grep foo | wc -l` produces three [`ParsedCommand`]s.
#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<ParsedCommand>,
}

impl Pipeline {
    /// Parse a line that may contain pipes.
    ///
    /// The input should already be trimmed of leading/trailing whitespace.
    pub fn parse(line: &str) -> Self {
        let commands: Vec<ParsedCommand> = line
            .split('|')
            .map(|s| ParsedCommand::parse(s.trim()))
            .filter(|c| !c.name.is_empty())
            .collect();
        Self { commands }
    }

    /// `true` when the pipeline has exactly one command (no pipe).
    pub fn is_simple(&self) -> bool {
        self.commands.len() <= 1
    }
}

impl fmt::Display for Pipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, cmd) in self.commands.iter().enumerate() {
            if i > 0 {
                write!(f, " | ")?;
            }
            write!(f, "{}", cmd)?;
        }
        Ok(())
    }
}

/// A parsed command
#[derive(Debug, Clone)]
pub struct ParsedCommand {
    /// The command name (first token)
    pub name: String,
    /// Arguments (remaining tokens)
    pub args: Vec<String>,
}

impl ParsedCommand {
    /// Parse a command line string.
    ///
    /// The input should already be trimmed of leading/trailing whitespace.
    pub fn parse(line: &str) -> Self {
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap_or("").to_string();
        let args: Vec<String> = parts.map(|s| s.to_string()).collect();
        Self { name, args }
    }

    /// Get all tokens as `&[&str]` (args[0] is the command name).
    pub fn args_slice(&self) -> Vec<&str> {
        let mut result = Vec::with_capacity(self.args.len() + 1);
        result.push(&*self.name);
        result.extend(self.args.iter().map(|s| s.as_str()));
        result
    }
}

impl fmt::Display for ParsedCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        for arg in &self.args {
            write!(f, " {}", arg)?;
        }
        Ok(())
    }
}