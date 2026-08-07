use alloc::fmt;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<ParsedCommand>,
}

impl Pipeline {
    pub fn parse(line: &str) -> Self {
        let commands: Vec<ParsedCommand> = split_pipeline(line)
            .iter()
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
    /// Target filename for `>` redirect, if present.
    pub redirect: Option<String>,
}

impl ParsedCommand {
    pub fn parse(line: &str) -> Self {
        let mut parts = shell_words(line);

        // Check for "> filename" redirect at the end of the command.
        let redirect = if parts.len() >= 3 && parts[parts.len() - 2] == ">" {
            let file = parts.pop().unwrap(); // filename
            parts.pop(); // ">"
            Some(file)
        } else {
            None
        };

        let name = parts.first().cloned().unwrap_or_default();
        let args: Vec<String> = parts.into_iter().skip(1).collect();
        Self {
            name,
            args,
            redirect,
        }
    }

    pub fn args_slice(&self) -> Vec<&str> {
        let mut result = Vec::with_capacity(self.args.len() + 1);
        result.push(&*self.name);
        result.extend(self.args.iter().map(|s| s.as_str()));
        result
    }
}

/// Split at unquoted pipeline separators. `|` inside a grep -E pattern is
/// alternation, not a shell pipeline.
fn split_pipeline(line: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            current.push(ch);
            escaped = true;
            continue;
        }
        if ch == '\'' || ch == '"' {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
            current.push(ch);
        } else if ch == '|' && quote.is_none() {
            result.push(current);
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    result.push(current);
    result
}

/// Tokenize one command and remove single/double quotes.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut in_word = false;

    for ch in line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            in_word = true;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            in_word = true;
            continue;
        }
        if ch == '\'' || ch == '"' {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
            in_word = true;
        } else if ch.is_whitespace() && quote.is_none() {
            if in_word {
                words.push(core::mem::take(&mut current));
                in_word = false;
            }
        } else {
            current.push(ch);
            in_word = true;
        }
    }
    if escaped {
        current.push('\\');
    }
    if in_word {
        words.push(current);
    }
    words
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

    #[test]
    fn keeps_quoted_regex_alternation_inside_grep() {
        let pipeline = Pipeline::parse(
            "dmesg | grep -E 'iwlwifi|WiFi|PCI|PciHealth|AER|MMIO' > /mnt/usb/wifi-debug.txt",
        );
        assert_eq!(pipeline.commands.len(), 2);
        assert_eq!(pipeline.commands[1].name, "grep");
        assert_eq!(
            pipeline.commands[1].args,
            ["-E", "iwlwifi|WiFi|PCI|PciHealth|AER|MMIO"]
        );
        assert_eq!(
            pipeline.commands[1].redirect.as_deref(),
            Some("/mnt/usb/wifi-debug.txt")
        );
    }

    #[test]
    fn preserves_literal_pipe_inside_double_quotes() {
        let pipeline = Pipeline::parse("echo \"a|b\" | grep -E \"a\\|b\"");
        assert_eq!(pipeline.commands.len(), 2);
        assert_eq!(pipeline.commands[0].args, ["a|b"]);
        assert_eq!(pipeline.commands[1].args, ["-E", "a|b"]);
    }
}
