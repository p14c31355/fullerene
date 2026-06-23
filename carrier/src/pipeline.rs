use alloc::fmt;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<ParsedCommand>,
}

impl Pipeline {
    pub fn parse(line: &str) -> Self {
        let commands: Vec<ParsedCommand> = line
            .split('|')
            .map(|s| ParsedCommand::parse(s.trim()))
            .filter(|c| !c.name.is_empty())
            .collect();
        Self { commands }
    }

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

#[derive(Debug, Clone)]
pub struct ParsedCommand {
    pub name: String,
    pub args: Vec<String>,
}

impl ParsedCommand {
    pub fn parse(line: &str) -> Self {
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap_or("").to_string();
        let args: Vec<String> = parts.map(|s| s.to_string()).collect();
        Self { name, args }
    }

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
