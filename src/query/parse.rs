//! Hand-written tokenizer + recursive-descent/Pratt parser for the supported
//! OQL subset. No parser-generator dependency; the grammar is small and fixed.

use crate::query::QueryError;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Ident(String),      // keywords and dotted class/field names (case preserved)
    At(String),         // @attr, stored without the leading @
    Int(i64),
    Float(f64),
    Str(String),
    Star,
    LParen,
    RParen,
    Comma,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Split query text into tokens. Identifiers may contain `.` and `*` (for
/// `com.acme.*` class globs) and `$` (inner classes). Strings are double-quoted.
pub fn tokenize(src: &str) -> Result<Vec<Token>, QueryError> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            ' ' | '\t' | '\n' | '\r' => { i += 1; }
            '(' => { out.push(Token::LParen); i += 1; }
            ')' => { out.push(Token::RParen); i += 1; }
            ',' => { out.push(Token::Comma); i += 1; }
            '=' => { out.push(Token::Eq); i += 1; }
            '!' if bytes.get(i + 1) == Some(&b'=') => { out.push(Token::Ne); i += 2; }
            '<' if bytes.get(i + 1) == Some(&b'=') => { out.push(Token::Le); i += 2; }
            '<' => { out.push(Token::Lt); i += 1; }
            '>' if bytes.get(i + 1) == Some(&b'=') => { out.push(Token::Ge); i += 2; }
            '>' => { out.push(Token::Gt); i += 1; }
            '@' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && is_ident_byte(bytes[j]) { j += 1; }
                if j == start {
                    return Err(QueryError("empty @attribute".into()));
                }
                out.push(Token::At(src[start..j].to_string()));
                i = j;
            }
            '"' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'"' { j += 1; }
                if j >= bytes.len() {
                    return Err(QueryError("unterminated string literal".into()));
                }
                out.push(Token::Str(src[start..j].to_string()));
                i = j + 1;
            }
            '*' => { out.push(Token::Star); i += 1; }
            c if c.is_ascii_digit()
                || (c == '-' && bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit())) =>
            {
                let start = i;
                let mut j = i + 1;
                let mut is_float = false;
                while j < bytes.len() {
                    let b = bytes[j];
                    if b.is_ascii_digit() { j += 1; }
                    else if b == b'.' && !is_float { is_float = true; j += 1; }
                    else { break; }
                }
                let text = &src[start..j];
                if is_float {
                    out.push(Token::Float(text.parse().map_err(|_| {
                        QueryError(format!("bad number: {text}"))
                    })?));
                } else {
                    out.push(Token::Int(text.parse().map_err(|_| {
                        QueryError(format!("bad number: {text}"))
                    })?));
                }
                i = j;
            }
            c if is_ident_start(c as u8) => {
                let start = i;
                let mut j = i;
                while j < bytes.len() && (is_ident_byte(bytes[j]) || bytes[j] == b'*') { j += 1; }
                out.push(Token::Ident(src[start..j].to_string()));
                i = j;
            }
            other => return Err(QueryError(format!("unexpected character '{other}'"))),
        }
    }
    Ok(out)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_simple_select() {
        let toks = tokenize("SELECT * FROM java.lang.String s").unwrap();
        assert_eq!(
            toks,
            vec![
                Token::Ident("SELECT".into()),
                Token::Star,
                Token::Ident("FROM".into()),
                Token::Ident("java.lang.String".into()),
                Token::Ident("s".into()),
            ]
        );
    }

    #[test]
    fn tokenize_attr_and_string_literal() {
        let toks = tokenize("WHERE @usedHeapSize > 100 AND name = \"foo\"").unwrap();
        assert_eq!(toks[0], Token::Ident("WHERE".into()));
        assert_eq!(toks[1], Token::At("usedHeapSize".into()));
        assert_eq!(toks[2], Token::Gt);
        assert_eq!(toks[3], Token::Int(100));
        assert_eq!(toks[4], Token::Ident("AND".into()));
        assert_eq!(toks[5], Token::Ident("name".into()));
        assert_eq!(toks[6], Token::Eq);
        assert_eq!(toks[7], Token::Str("foo".into()));
    }
}
