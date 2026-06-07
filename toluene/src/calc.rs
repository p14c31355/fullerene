//! Calculator engine for the Toluene SDK.
//!
//! Provides a simple four-function calculator with expression evaluation.
//! Supports `+`, `-`, `*`, `/` and parentheses.

extern crate alloc;

/// Evaluate a simple arithmetic expression string and return the result.
///
/// Supported operators: `+`, `-`, `*`, `/`.
/// Parentheses `()` are supported for grouping.
/// Returns `None` on parse error or division by zero.
pub fn evaluate(expr: &str) -> Option<i64> {
    let tokens = tokenize(expr)?;
    let (result, index) = parse_expr(&tokens, 0)?;
    if index == tokens.len() {
        Some(result)
    } else {
        None
    }
}

/// Token types.
#[derive(Debug, Clone, Copy)]
enum Token {
    Num(i64),
    Plus,
    Minus,
    Mul,
    Div,
    LParen,
    RParen,
}

/// Tokenize an expression string.
fn tokenize(expr: &str) -> Option<alloc::vec::Vec<Token>> {
    let mut tokens = alloc::vec::Vec::new();
    let bytes = expr.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let ch = bytes[i];
        match ch {
            b' ' | b'\t' | b'\n' | b'\r' => {
                i += 1;
                continue;
            }
            b'0'..=b'9' => {
                let mut val: i64 = 0;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    val = match val.checked_mul(10).and_then(|v| v.checked_add((bytes[i] - b'0') as i64)) {
                        Some(v) => v,
                        None => return None, // Overflow
                    };
                    i += 1;
                }
                tokens.push(Token::Num(val));
            }
            b'+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            b'-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            b'*' => {
                tokens.push(Token::Mul);
                i += 1;
            }
            b'/' => {
                tokens.push(Token::Div);
                i += 1;
            }
            b'(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            b')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            _ => return None, // Invalid character
        }
    }
    Some(tokens)
}

/// Parse an addition/subtraction expression.
fn parse_expr(tokens: &[Token], pos: usize) -> Option<(i64, usize)> {
    let (mut left, mut pos) = parse_term(tokens, pos)?;

    while pos < tokens.len() {
        match tokens[pos] {
            Token::Plus => {
                let (right, new_pos) = parse_term(tokens, pos + 1)?;
                left = left.checked_add(right)?;
                pos = new_pos;
            }
            Token::Minus => {
                let (right, new_pos) = parse_term(tokens, pos + 1)?;
                left = left.checked_sub(right)?;
                pos = new_pos;
            }
            _ => break,
        }
    }
    Some((left, pos))
}

/// Parse a multiplication/division term.
fn parse_term(tokens: &[Token], pos: usize) -> Option<(i64, usize)> {
    let (mut left, mut pos) = parse_factor(tokens, pos)?;

    while pos < tokens.len() {
        match tokens[pos] {
            Token::Mul => {
                let (right, new_pos) = parse_factor(tokens, pos + 1)?;
                left = left.checked_mul(right)?;
                pos = new_pos;
            }
            Token::Div => {
                let (right, new_pos) = parse_factor(tokens, pos + 1)?;
                if right == 0 {
                    return None;
                }
                left = left.checked_div(right)?;
                pos = new_pos;
            }
            _ => break,
        }
    }
    Some((left, pos))
}

/// Parse a factor (number, parenthesized expression, or unary minus).
fn parse_factor(tokens: &[Token], pos: usize) -> Option<(i64, usize)> {
    if pos >= tokens.len() {
        return None;
    }
    match tokens[pos] {
        Token::Num(n) => Some((n, pos + 1)),
        Token::Minus => {
            let (val, new_pos) = parse_factor(tokens, pos + 1)?;
            Some((val.checked_neg()?, new_pos))
        }
        Token::LParen => {
            let (val, new_pos) = parse_expr(tokens, pos + 1)?;
            if new_pos < tokens.len() && matches!(tokens[new_pos], Token::RParen) {
                Some((val, new_pos + 1))
            } else {
                None // Missing closing paren
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_ops() {
        assert_eq!(evaluate("1+2"), Some(3));
        assert_eq!(evaluate("5-3"), Some(2));
        assert_eq!(evaluate("4*3"), Some(12));
        assert_eq!(evaluate("8/2"), Some(4));
    }

    #[test]
    fn test_precedence() {
        assert_eq!(evaluate("2+3*4"), Some(14));
        assert_eq!(evaluate("10-6/2"), Some(7));
    }

    #[test]
    fn test_parentheses() {
        assert_eq!(evaluate("(2+3)*4"), Some(20));
        assert_eq!(evaluate("2*(3+4)"), Some(14));
    }

    #[test]
    fn test_unary_minus() {
        assert_eq!(evaluate("-5+3"), Some(-2));
        assert_eq!(evaluate("(-3)"), Some(-3));
    }

    #[test]
    fn test_division_by_zero() {
        assert_eq!(evaluate("5/0"), None);
    }
}