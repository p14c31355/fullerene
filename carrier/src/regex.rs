//! A small no-std extended regular-expression matcher for shell commands.
//!
//! This intentionally implements the useful line-oriented subset needed by
//! `grep -E`: literals, `.`, `^`, `$`, character classes/ranges, grouping,
//! alternation, and the `*`, `+`, and `?` repetition operators.

use alloc::boxed::Box;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegexError {
    EmptyPattern,
    UnexpectedEnd,
    UnclosedGroup,
    UnclosedClass,
    InvalidRange,
    RepetitionWithoutAtom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrepArgs<'a> {
    pub extended: bool,
    pub pattern: &'a str,
    pub first_file: usize,
}

/// Parse the common grep arguments shared by the Nozzle and kernel paths.
/// `args[0]` is the command name.
pub fn parse_grep_args<'a>(args: &'a [&'a str]) -> Option<GrepArgs<'a>> {
    let mut index = 1;
    let mut extended = false;
    while let Some(arg) = args.get(index).copied() {
        match arg {
            "-E" | "--extended-regexp" => {
                extended = true;
                index += 1;
            }
            "--" => {
                index += 1;
                break;
            }
            _ => break,
        }
    }
    let pattern = *args.get(index)?;
    Some(GrepArgs {
        extended,
        pattern,
        first_file: index + 1,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repeat {
    ZeroOrMore,
    OneOrMore,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Expr {
    Empty,
    Literal(u8),
    Any,
    Class {
        negated: bool,
        ranges: Vec<(u8, u8)>,
    },
    Start,
    End,
    Concat(Vec<Expr>),
    Alt(Vec<Expr>),
    Repeat(Box<Expr>, Repeat),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regex {
    expr: Expr,
}

impl Regex {
    pub fn new(pattern: &str) -> Result<Self, RegexError> {
        if pattern.is_empty() {
            return Err(RegexError::EmptyPattern);
        }
        let mut parser = Parser {
            input: pattern.as_bytes(),
            pos: 0,
        };
        let expr = parser.parse_alt(false)?;
        if parser.pos != parser.input.len() {
            return Err(RegexError::UnexpectedEnd);
        }
        Ok(Self { expr })
    }

    /// Match anywhere in one input line, as grep does by default.
    pub fn is_match(&self, text: &str) -> bool {
        let input = text.as_bytes();
        for start in 0..=input.len() {
            let mut out = Vec::new();
            match_positions(&self.expr, input, start, &mut out);
            if !out.is_empty() {
                return true;
            }
        }
        false
    }
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn parse_alt(&mut self, in_group: bool) -> Result<Expr, RegexError> {
        let mut alternatives = Vec::new();
        alternatives.push(self.parse_concat(in_group)?);
        while self.take_if(b'|') {
            alternatives.push(self.parse_concat(in_group)?);
        }
        if alternatives.len() == 1 {
            Ok(alternatives.pop().unwrap_or(Expr::Empty))
        } else {
            Ok(Expr::Alt(alternatives))
        }
    }

    fn parse_concat(&mut self, in_group: bool) -> Result<Expr, RegexError> {
        let mut parts = Vec::new();
        while let Some(&byte) = self.input.get(self.pos) {
            if byte == b'|' || (in_group && byte == b')') {
                break;
            }
            parts.push(self.parse_repeated_atom()?);
        }
        if parts.len() == 1 {
            Ok(parts.pop().unwrap_or(Expr::Empty))
        } else {
            Ok(Expr::Concat(parts))
        }
    }

    fn parse_repeated_atom(&mut self) -> Result<Expr, RegexError> {
        let mut atom = self.parse_atom()?;
        let repeat = match self.input.get(self.pos).copied() {
            Some(b'*') => Some(Repeat::ZeroOrMore),
            Some(b'+') => Some(Repeat::OneOrMore),
            Some(b'?') => Some(Repeat::Optional),
            _ => None,
        };
        if repeat.is_some() {
            self.pos += 1;
            atom = Expr::Repeat(Box::new(atom), repeat.unwrap_or(Repeat::Optional));
        }
        Ok(atom)
    }

    fn parse_atom(&mut self) -> Result<Expr, RegexError> {
        let byte = self.input.get(self.pos).copied();
        match byte {
            None => Err(RegexError::UnexpectedEnd),
            Some(b'(') => {
                self.pos += 1;
                let expr = self.parse_alt(true)?;
                if !self.take_if(b')') {
                    return Err(RegexError::UnclosedGroup);
                }
                Ok(expr)
            }
            Some(b'[') => self.parse_class(),
            Some(b'.') => {
                self.pos += 1;
                Ok(Expr::Any)
            }
            Some(b'^') => {
                self.pos += 1;
                Ok(Expr::Start)
            }
            Some(b'$') => {
                self.pos += 1;
                Ok(Expr::End)
            }
            Some(b'\\') => {
                self.pos += 1;
                let escaped = self
                    .input
                    .get(self.pos)
                    .copied()
                    .ok_or(RegexError::UnexpectedEnd)?;
                self.pos += 1;
                Ok(Expr::Literal(match escaped {
                    b'n' => b'\n',
                    b't' => b'\t',
                    other => other,
                }))
            }
            Some(b'*' | b'+' | b'?') => Err(RegexError::RepetitionWithoutAtom),
            Some(other) => {
                self.pos += 1;
                Ok(Expr::Literal(other))
            }
        }
    }

    fn parse_class(&mut self) -> Result<Expr, RegexError> {
        self.pos += 1; // '['
        let negated = self.take_if(b'^');
        let mut ranges = Vec::new();
        let mut first = true;
        while let Some(&byte) = self.input.get(self.pos) {
            if byte == b']' && !first {
                self.pos += 1;
                return Ok(Expr::Class { negated, ranges });
            }
            let start = self.parse_class_char()?;
            first = false;
            if self.take_if(b'-') {
                if self.input.get(self.pos) == Some(&b']') || self.input.get(self.pos).is_none() {
                    ranges.push((start, b'-'));
                    continue;
                }
                let end = self.parse_class_char()?;
                if start > end {
                    return Err(RegexError::InvalidRange);
                }
                ranges.push((start, end));
            } else {
                ranges.push((start, start));
            }
        }
        Err(RegexError::UnclosedClass)
    }

    fn parse_class_char(&mut self) -> Result<u8, RegexError> {
        let byte = self
            .input
            .get(self.pos)
            .copied()
            .ok_or(RegexError::UnclosedClass)?;
        if byte == b'\\' {
            self.pos += 1;
            let escaped = self
                .input
                .get(self.pos)
                .copied()
                .ok_or(RegexError::UnclosedClass)?;
            self.pos += 1;
            Ok(match escaped {
                b'n' => b'\n',
                b't' => b'\t',
                other => other,
            })
        } else {
            self.pos += 1;
            Ok(byte)
        }
    }

    fn take_if(&mut self, expected: u8) -> bool {
        if self.input.get(self.pos) == Some(&expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
}

fn match_positions(expr: &Expr, input: &[u8], pos: usize, out: &mut Vec<usize>) {
    match expr {
        Expr::Empty => push_unique(out, pos),
        Expr::Literal(expected) => {
            if input.get(pos) == Some(expected) {
                push_unique(out, pos + 1);
            }
        }
        Expr::Any => {
            if pos < input.len() {
                push_unique(out, pos + 1);
            }
        }
        Expr::Class { negated, ranges } => {
            if let Some(&byte) = input.get(pos) {
                let found = ranges
                    .iter()
                    .any(|&(start, end)| (start..=end).contains(&byte));
                if found != *negated {
                    push_unique(out, pos + 1);
                }
            }
        }
        Expr::Start => {
            if pos == 0 {
                push_unique(out, pos);
            }
        }
        Expr::End => {
            if pos == input.len() {
                push_unique(out, pos);
            }
        }
        Expr::Concat(parts) => {
            let mut positions = alloc::vec![pos];
            for part in parts {
                let mut next = Vec::new();
                for position in positions {
                    match_positions(part, input, position, &mut next);
                }
                positions = next;
                if positions.is_empty() {
                    break;
                }
            }
            for position in positions {
                push_unique(out, position);
            }
        }
        Expr::Alt(alternatives) => {
            for alternative in alternatives {
                match_positions(alternative, input, pos, out);
            }
        }
        Expr::Repeat(inner, Repeat::Optional) => {
            push_unique(out, pos);
            match_positions(inner, input, pos, out);
        }
        Expr::Repeat(inner, Repeat::OneOrMore) => {
            let mut first = Vec::new();
            match_positions(inner, input, pos, &mut first);
            for position in first.iter().copied() {
                push_unique(out, position);
            }
            repeat_closure(inner, input, first, out);
        }
        Expr::Repeat(inner, Repeat::ZeroOrMore) => {
            push_unique(out, pos);
            repeat_closure(inner, input, alloc::vec![pos], out);
        }
    }
}

fn repeat_closure(inner: &Expr, input: &[u8], mut frontier: Vec<usize>, out: &mut Vec<usize>) {
    while let Some(position) = frontier.pop() {
        let mut next = Vec::new();
        match_positions(inner, input, position, &mut next);
        for candidate in next {
            if !out.contains(&candidate) {
                out.push(candidate);
                frontier.push(candidate);
            }
        }
    }
}

fn push_unique(values: &mut Vec<usize>, value: usize) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::Regex;

    fn matches(pattern: &str, text: &str) -> bool {
        Regex::new(pattern).unwrap().is_match(text)
    }

    #[test]
    fn supports_alternation_and_groups() {
        assert!(matches("iwlwifi|PciHealth", "PciHealth: link down"));
        assert!(matches("(foo|bar)+", "xxbarfoo"));
        assert!(!matches("iwlwifi|PciHealth", "MMIO only"));
    }

    #[test]
    fn supports_classes_ranges_and_repetition() {
        assert!(matches("[0-9a-f]+", "rev=0061"));
        assert!(matches("AER?", "AE"));
        assert!(matches("MMIO.*", "MMIO phase aborted"));
    }

    #[test]
    fn supports_anchors_and_escaped_literals() {
        assert!(matches("^iwlwifi:", "iwlwifi: init.begin"));
        assert!(matches("disabled$", "init.result status=disabled"));
        assert!(matches(r"a\.b", "a.b"));
        assert!(!matches("^iwlwifi:", "prefix iwlwifi:"));
    }

    #[test]
    fn rejects_malformed_patterns() {
        assert!(Regex::new("").is_err());
        assert!(Regex::new("[").is_err());
        assert!(Regex::new("(").is_err());
        assert!(Regex::new("*").is_err());
    }
}
