//! Simple command-line parser for Nozzle
//!
//! Splits input into command name and arguments.
//! Currently supports whitespace splitting; quoting support can be added later.

use alloc::fmt;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

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