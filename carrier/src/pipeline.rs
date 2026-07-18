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
        f.write_str(&self.name)?;
        self.args.iter().try_for_each(|a| write!(f, " {a}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_command() {
        let pipeline = Pipeline::parse("echo hello");
        assert!(!pipeline.commands.is_empty());
        assert_eq!(pipeline.commands[0].name, "echo");
        assert_eq!(pipeline.commands[0].args.len(), 1);
        assert_eq!(pipeline.commands[0].args[0], "hello");
    }

    #[test]
    fn test_parse_pipeline() {
        let pipeline = Pipeline::parse("cat file | grep foo");
        assert_eq!(pipeline.commands.len(), 2);
        assert_eq!(pipeline.commands[0].name, "cat");
        assert_eq!(pipeline.commands[1].name, "grep");
    }

    #[test]
    fn test_parse_empty() {
        let pipeline = Pipeline::parse("");
        assert!(pipeline.commands.is_empty());
    }

    #[test]
    fn test_is_simple() {
        let simple = Pipeline::parse("echo hello");
        assert!(simple.is_simple());
        let multi = Pipeline::parse("cat | grep");
        assert!(!multi.is_simple());
    }

    #[test]
    fn test_args_slice() {
        let pipeline = Pipeline::parse("ls -la /tmp");
        let args = pipeline.commands[0].args_slice();
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "ls");
        assert_eq!(args[1], "-la");
        assert_eq!(args[2], "/tmp");
    }

    #[test]
    fn test_parse_whitespace() {
        let pipeline = Pipeline::parse("   echo   foo   ");
        assert_eq!(pipeline.commands.len(), 1);
        assert_eq!(pipeline.commands[0].name, "echo");
        assert_eq!(pipeline.commands[0].args.len(), 1);
        assert_eq!(pipeline.commands[0].args[0], "foo");
    }

    #[test]
    fn test_parse_without_args() {
        let pipeline = Pipeline::parse("ls");
        assert_eq!(pipeline.commands.len(), 1);
        assert_eq!(pipeline.commands[0].name, "ls");
        assert!(pipeline.commands[0].args.is_empty());
    }
}
