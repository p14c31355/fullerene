//! A small no-std extended regular-expression matcher for shell commands.
//!
//! This intentionally implements the useful line-oriented subset needed by
//! `grep -E`: literals, `.`, `^`, `$`, character classes/ranges, grouping,
//! alternation, and the `*`, `+`, and `?` repetition operators. `Expr::Any`
//! and character classes operate on bytes; for non-ASCII UTF-8 input, `.`
//! therefore matches one byte rather than one Unicode character. `is_match`
//! returns only a boolean, so it does not expose any invalid slicing that may
//! result from a byte-oriented match.

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
    TooDeep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrepArgs<'a> {
    pub extended: bool,
    pub pattern: &'a str,
    pub first_file: usize,
}

/// The compiled matcher shared by the shell and kernel grep implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher<'a> {
    Literal(&'a str),
    Extended(Regex),
}

impl Matcher<'_> {
    pub fn is_match(&self, text: &str) -> bool {
        match self {
            Self::Literal(pattern) => text.contains(pattern),
            Self::Extended(regex) => regex.is_match(text),
        }
    }
}

impl<'a> GrepArgs<'a> {
    pub fn compile(self) -> Result<Matcher<'a>, RegexError> {
        if self.extended {
            Regex::new(self.pattern).map(Matcher::Extended)
        } else {
            Ok(Matcher::Literal(self.pattern))
        }
    }
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
            depth: 0,
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
        let mut scratch = Vec::new();
        for start in 0..=input.len() {
            scratch.clear();
            if match_positions(&self.expr, input, start, &mut scratch, 0, true) {
                return true;
            }
        }
        false
    }
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    depth: usize,
}

const MAX_DEPTH: usize = 32;

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
        if let Some(repeat) = match self.input.get(self.pos).copied() {
            Some(b'*') => Some(Repeat::ZeroOrMore),
            Some(b'+') => Some(Repeat::OneOrMore),
            Some(b'?') => Some(Repeat::Optional),
            _ => None,
        } {
            self.pos += 1;
            atom = Expr::Repeat(Box::new(atom), repeat);
        }
        Ok(atom)
    }

    fn parse_atom(&mut self) -> Result<Expr, RegexError> {
        let byte = self.input.get(self.pos).copied();
        match byte {
            None => Err(RegexError::UnexpectedEnd),
            Some(b'(') => {
                self.pos += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    self.depth -= 1;
                    return Err(RegexError::TooDeep);
                }
                let parsed = self.parse_alt(true);
                self.depth -= 1;
                let expr = parsed?;
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
                    ranges.push((start, start));
                    ranges.push((b'-', b'-'));
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

fn match_positions(
    expr: &Expr,
    input: &[u8],
    pos: usize,
    out: &mut Vec<usize>,
    depth: usize,
    any_match: bool,
) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match expr {
        Expr::Empty => {
            if !any_match {
                push_unique(out, pos);
            }
            true
        }
        Expr::Literal(expected) => {
            if input.get(pos) == Some(expected) {
                if !any_match {
                    push_unique(out, pos + 1);
                }
                return true;
            }
            false
        }
        Expr::Any => {
            if pos < input.len() {
                if !any_match {
                    push_unique(out, pos + 1);
                }
                return true;
            }
            false
        }
        Expr::Class { negated, ranges } => {
            if let Some(&byte) = input.get(pos) {
                let found = ranges
                    .iter()
                    .any(|&(start, end)| (start..=end).contains(&byte));
                if found != *negated {
                    if !any_match {
                        push_unique(out, pos + 1);
                    }
                    return true;
                }
            }
            false
        }
        Expr::Start => {
            if pos == 0 {
                if !any_match {
                    push_unique(out, pos);
                }
                return true;
            }
            false
        }
        Expr::End => {
            if pos == input.len() {
                if !any_match {
                    push_unique(out, pos);
                }
                return true;
            }
            false
        }
        Expr::Concat(parts) => {
            let mut positions = alloc::vec![pos];
            for part in parts {
                let mut next = Vec::new();
                for position in positions {
                    match_positions(part, input, position, &mut next, depth + 1, false);
                }
                positions = next;
                if positions.is_empty() {
                    break;
                }
            }
            if any_match {
                !positions.is_empty()
            } else {
                for position in positions {
                    push_unique(out, position);
                }
                false
            }
        }
        Expr::Alt(alternatives) => {
            for alternative in alternatives {
                if match_positions(alternative, input, pos, out, depth + 1, any_match) && any_match
                {
                    return true;
                }
            }
            false
        }
        Expr::Repeat(inner, Repeat::Optional) => {
            if any_match {
                true
            } else {
                push_unique(out, pos);
                match_positions(inner, input, pos, out, depth + 1, false);
                false
            }
        }
        Expr::Repeat(inner, Repeat::OneOrMore) => {
            if any_match {
                return match_positions(inner, input, pos, out, depth + 1, true);
            }
            let mut first = Vec::new();
            match_positions(inner, input, pos, &mut first, depth + 1, false);
            for position in first.iter().copied() {
                push_unique(out, position);
            }
            repeat_closure(inner, input, first, out, depth + 1);
            false
        }
        Expr::Repeat(inner, Repeat::ZeroOrMore) => {
            if any_match {
                true
            } else {
                push_unique(out, pos);
                repeat_closure(inner, input, alloc::vec![pos], out, depth + 1);
                false
            }
        }
    }
}

fn repeat_closure(
    inner: &Expr,
    input: &[u8],
    mut frontier: Vec<usize>,
    out: &mut Vec<usize>,
    depth: usize,
) {
    let mut visited = frontier.clone();
    while let Some(position) = frontier.pop() {
        let mut next = Vec::new();
        match_positions(inner, input, position, &mut next, depth + 1, false);
        for candidate in next {
            if !visited.contains(&candidate) {
                visited.push(candidate);
                push_unique(out, candidate);
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
    use super::{Regex, RegexError, parse_grep_args};

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
        assert!(matches("[a-]", "a"));
        assert!(matches("[a-]", "-"));
        assert!(matches("[^0-9]+", "abc"));
        assert!(matches("AER?", "AE"));
        assert!(matches("MMIO.*", "MMIO phase aborted"));
    }

    #[test]
    fn expands_repetition_inside_alternation() {
        assert!(matches("^(aa|a+)b$", "aaab"));
    }

    #[test]
    fn rejects_excessive_group_nesting() {
        let pattern = alloc::format!("{}a{}", "(".repeat(33), ")".repeat(33));
        assert_eq!(Regex::new(&pattern), Err(RegexError::TooDeep));
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

    #[test]
    fn parses_grep_options() {
        let args = ["grep", "-E", "foo", "a.log"];
        let parsed = parse_grep_args(&args).unwrap();
        assert!(parsed.extended);
        assert_eq!(parsed.pattern, "foo");
        assert_eq!(parsed.first_file, 3);

        let args = ["grep", "--extended-regexp", "--", "-foo", "a.log"];
        let parsed = parse_grep_args(&args).unwrap();
        assert!(parsed.extended);
        assert_eq!(parsed.pattern, "-foo");
        assert_eq!(parsed.first_file, 4);
        assert!(parse_grep_args(&["grep"]).is_none());
    }
}
