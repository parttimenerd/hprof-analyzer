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

use crate::query::ast::{
    AggFunc, Attr, ClassSpec, CompareOp, Predicate, Query, SelectItem, Value,
};

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

/// Parse a full query. Errors carry a human-readable message.
pub fn parse(src: &str) -> Result<Query, QueryError> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, pos: 0 };
    let q = p.query()?;
    if p.pos != p.toks.len() {
        return Err(QueryError(format!(
            "unexpected trailing token: {:?}",
            p.toks[p.pos]
        )));
    }
    Ok(q)
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }
    fn bump(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    /// Consume an identifier equal (case-insensitively) to `kw`.
    fn eat_kw(&mut self, kw: &str) -> bool {
        if let Some(Token::Ident(s)) = self.peek() {
            if s.eq_ignore_ascii_case(kw) {
                self.pos += 1;
                return true;
            }
        }
        false
    }
    fn expect_kw(&mut self, kw: &str) -> Result<(), QueryError> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(QueryError(format!("expected `{kw}`, found {:?}", self.peek())))
        }
    }

    fn query(&mut self) -> Result<Query, QueryError> {
        self.expect_kw("SELECT")?;
        let distinct = self.eat_kw("DISTINCT");
        let select = self.select_list()?;
        self.expect_kw("FROM")?;
        let instanceof = self.eat_kw("INSTANCEOF");
        let class_name = match self.bump() {
            Some(Token::Ident(s)) => s,
            other => return Err(QueryError(format!("expected class name, found {other:?}"))),
        };
        // Optional alias: a bare identifier that is not a reserved keyword.
        let alias = match self.peek() {
            Some(Token::Ident(s))
                if !is_reserved(s) =>
            {
                let a = s.clone();
                self.pos += 1;
                Some(a)
            }
            _ => None,
        };
        let where_ = if self.eat_kw("WHERE") {
            Some(self.pred_or()?)
        } else {
            None
        };
        let limit = if self.eat_kw("LIMIT") {
            match self.bump() {
                Some(Token::Int(n)) if n >= 0 => Some(n as u64),
                other => return Err(QueryError(format!("expected LIMIT count, found {other:?}"))),
            }
        } else {
            None
        };
        Ok(Query { distinct, select, from: ClassSpec { instanceof, class_name }, alias, where_, limit })
    }

    fn select_list(&mut self) -> Result<Vec<SelectItem>, QueryError> {
        let mut items = vec![self.select_item()?];
        while matches!(self.peek(), Some(Token::Comma)) {
            self.pos += 1;
            items.push(self.select_item()?);
        }
        Ok(items)
    }

    fn select_item(&mut self) -> Result<SelectItem, QueryError> {
        // Aggregate? COUNT/SUM/MIN/MAX/AVG '(' item ')'
        if let Some(Token::Ident(s)) = self.peek() {
            if let Some(func) = agg_func(s) {
                self.pos += 1;
                self.expect_lparen()?;
                let arg = Box::new(self.select_item()?);
                self.expect_rparen()?;
                return Ok(SelectItem::Aggregate { func, arg });
            }
        }
        if matches!(self.peek(), Some(Token::Star)) {
            self.pos += 1;
            return Ok(SelectItem::Star);
        }
        Ok(SelectItem::Attr(self.attr()?))
    }

    fn attr(&mut self) -> Result<Attr, QueryError> {
        match self.bump() {
            Some(Token::At(name)) => Ok(match name.as_str() {
                "objectId" => Attr::ObjectId,
                "objectAddress" => Attr::ObjectAddress,
                "usedHeapSize" => Attr::UsedHeapSize,
                "displayName" => Attr::DisplayName,
                "length" => Attr::Length,
                other => return Err(QueryError(format!("unknown @attribute: @{other}"))),
            }),
            Some(Token::Ident(s)) if s.eq_ignore_ascii_case("classof") => {
                self.expect_lparen()?;
                // consume the single alias argument (ignored; classof is on the row)
                let _ = self.bump();
                self.expect_rparen()?;
                Ok(Attr::ClassOf)
            }
            Some(Token::Ident(s)) => Ok(Attr::Field(s)),
            other => Err(QueryError(format!("expected attribute, found {other:?}"))),
        }
    }

    // Pratt-ish predicate grammar: OR < AND < NOT < primary.
    fn pred_or(&mut self) -> Result<Predicate, QueryError> {
        let mut left = self.pred_and()?;
        while self.eat_kw("OR") {
            let right = self.pred_and()?;
            left = Predicate::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn pred_and(&mut self) -> Result<Predicate, QueryError> {
        let mut left = self.pred_not()?;
        while self.eat_kw("AND") {
            let right = self.pred_not()?;
            left = Predicate::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn pred_not(&mut self) -> Result<Predicate, QueryError> {
        if self.eat_kw("NOT") {
            return Ok(Predicate::Not(Box::new(self.pred_not()?)));
        }
        self.pred_primary()
    }
    fn pred_primary(&mut self) -> Result<Predicate, QueryError> {
        if matches!(self.peek(), Some(Token::LParen)) {
            self.pos += 1;
            let inner = self.pred_or()?;
            self.expect_rparen()?;
            return Ok(inner);
        }
        // `<alias-or-attr> INSTANCEOF C`  OR  `<attr> <op> <value>`
        let lhs = self.attr()?;
        if self.eat_kw("INSTANCEOF") {
            let cname = match self.bump() {
                Some(Token::Ident(s)) => s,
                other => return Err(QueryError(format!("expected class after INSTANCEOF, found {other:?}"))),
            };
            return Ok(Predicate::InstanceOf(cname));
        }
        let op = match self.bump() {
            Some(Token::Eq) => CompareOp::Eq,
            Some(Token::Ne) => CompareOp::Ne,
            Some(Token::Lt) => CompareOp::Lt,
            Some(Token::Le) => CompareOp::Le,
            Some(Token::Gt) => CompareOp::Gt,
            Some(Token::Ge) => CompareOp::Ge,
            other => return Err(QueryError(format!("expected comparison operator, found {other:?}"))),
        };
        let rhs = match self.bump() {
            Some(Token::Int(n)) => Value::Int(n),
            Some(Token::Float(f)) => Value::Float(f),
            Some(Token::Str(s)) => Value::Str(s),
            Some(Token::Ident(s)) if s.eq_ignore_ascii_case("true") => Value::Bool(true),
            Some(Token::Ident(s)) if s.eq_ignore_ascii_case("false") => Value::Bool(false),
            Some(Token::Ident(s)) if s.eq_ignore_ascii_case("null") => Value::Null,
            other => return Err(QueryError(format!("expected literal value, found {other:?}"))),
        };
        Ok(Predicate::Compare { lhs, op, rhs })
    }

    fn expect_lparen(&mut self) -> Result<(), QueryError> {
        match self.bump() {
            Some(Token::LParen) => Ok(()),
            other => Err(QueryError(format!("expected `(`, found {other:?}"))),
        }
    }
    fn expect_rparen(&mut self) -> Result<(), QueryError> {
        match self.bump() {
            Some(Token::RParen) => Ok(()),
            other => Err(QueryError(format!("expected `)`, found {other:?}"))),
        }
    }
}

fn agg_func(s: &str) -> Option<AggFunc> {
    match () {
        _ if s.eq_ignore_ascii_case("COUNT") => Some(AggFunc::Count),
        _ if s.eq_ignore_ascii_case("SUM") => Some(AggFunc::Sum),
        _ if s.eq_ignore_ascii_case("MIN") => Some(AggFunc::Min),
        _ if s.eq_ignore_ascii_case("MAX") => Some(AggFunc::Max),
        _ if s.eq_ignore_ascii_case("AVG") => Some(AggFunc::Avg),
        _ => None,
    }
}

fn is_reserved(s: &str) -> bool {
    ["WHERE", "LIMIT", "UNION", "AND", "OR", "NOT", "INSTANCEOF"]
        .iter()
        .any(|k| s.eq_ignore_ascii_case(k))
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

    use crate::query::ast::*;

    #[test]
    fn parse_star_from() {
        let q = parse("SELECT * FROM java.lang.String s").unwrap();
        assert_eq!(q.select, vec![SelectItem::Star]);
        assert_eq!(q.from.class_name, "java.lang.String");
        assert!(!q.from.instanceof);
        assert_eq!(q.alias.as_deref(), Some("s"));
        assert!(q.where_.is_none());
    }

    #[test]
    fn parse_instanceof_and_where_and_limit() {
        let q = parse(
            "SELECT @objectId, name FROM INSTANCEOF java.lang.Thread t \
             WHERE @usedHeapSize >= 100 AND name != \"main\" LIMIT 5",
        )
        .unwrap();
        assert!(q.from.instanceof);
        assert_eq!(q.limit, Some(5));
        assert_eq!(q.select.len(), 2);
        // WHERE is AND(Compare(usedHeapSize >= 100), Compare(name != "main"))
        match q.where_.unwrap() {
            Predicate::And(a, b) => {
                assert!(matches!(*a, Predicate::Compare { op: CompareOp::Ge, .. }));
                assert!(matches!(*b, Predicate::Compare { op: CompareOp::Ne, .. }));
            }
            other => panic!("expected AND, got {other:?}"),
        }
    }

    #[test]
    fn parse_aggregate() {
        let q = parse("SELECT COUNT(*) FROM java.lang.String").unwrap();
        assert!(matches!(
            q.select[0],
            SelectItem::Aggregate { func: AggFunc::Count, .. }
        ));
        assert!(q.alias.is_none());
    }

    #[test]
    fn parse_rejects_trailing_garbage() {
        let err = parse("SELECT * FROM C bogus extra tokens").unwrap_err();
        assert!(err.0.contains("unexpected"), "got: {}", err.0);
    }

    #[test]
    fn parse_or_and_not_precedence() {
        // NOT binds tighter than AND, AND tighter than OR.
        let q = parse("SELECT * FROM C WHERE NOT a = 1 OR b = 2 AND c = 3").unwrap();
        // => Or( Not(a=1), And(b=2, c=3) )
        match q.where_.unwrap() {
            Predicate::Or(l, r) => {
                assert!(matches!(*l, Predicate::Not(_)));
                assert!(matches!(*r, Predicate::And(_, _)));
            }
            other => panic!("expected OR at top, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_missing_from() {
        let err = parse("SELECT *").unwrap_err();
        assert!(err.0.contains("FROM"), "got: {}", err.0);
    }

    #[test]
    fn parse_rejects_unknown_attribute() {
        let err = parse("SELECT @bogus FROM C").unwrap_err();
        assert!(
            err.0.contains("unknown @attribute") && err.0.contains("bogus"),
            "got: {}",
            err.0
        );
    }

    #[test]
    fn parse_rejects_bad_comparison_operator() {
        // `name LIMIT 1` after WHERE: `LIMIT` is not a comparison operator, and
        // the tokenizer produces an Ident, so the operator match must reject it.
        let err = parse("SELECT * FROM C WHERE name name").unwrap_err();
        assert!(
            err.0.contains("comparison operator"),
            "got: {}",
            err.0
        );
    }

    #[test]
    fn parse_rejects_non_literal_rhs() {
        let err = parse("SELECT * FROM C WHERE a = @objectId").unwrap_err();
        assert!(err.0.contains("literal value"), "got: {}", err.0);
    }

    #[test]
    fn parse_rejects_negative_limit() {
        let err = parse("SELECT * FROM C LIMIT -1").unwrap_err();
        assert!(err.0.contains("LIMIT count"), "got: {}", err.0);
    }

    #[test]
    fn parse_aggregate_missing_paren_reports_found_token() {
        // COUNT without `(` should report what it found instead, not a bare message.
        let err = parse("SELECT COUNT * FROM C").unwrap_err();
        assert!(
            err.0.contains("expected `(`") && err.0.contains("found"),
            "got: {}",
            err.0
        );
    }

    #[test]
    fn parse_classof_attr() {
        let q = parse("SELECT classof(s) FROM java.lang.String s").unwrap();
        assert_eq!(q.select, vec![SelectItem::Attr(Attr::ClassOf)]);
    }

    #[test]
    fn parse_distinct_and_parenthesized_predicate() {
        let q = parse("SELECT DISTINCT name FROM C WHERE (a = 1 OR b = 2) AND c = 3").unwrap();
        assert!(q.distinct);
        // Top-level AND with left = Or(...), right = Compare(c = 3).
        match q.where_.unwrap() {
            Predicate::And(l, r) => {
                assert!(matches!(*l, Predicate::Or(_, _)));
                assert!(matches!(*r, Predicate::Compare { op: CompareOp::Eq, .. }));
            }
            other => panic!("expected AND at top, got {other:?}"),
        }
    }

    #[test]
    fn parse_predicate_instanceof() {
        let q = parse("SELECT * FROM C WHERE s INSTANCEOF java.lang.String").unwrap();
        match q.where_.unwrap() {
            Predicate::InstanceOf(name) => assert_eq!(name, "java.lang.String"),
            other => panic!("expected InstanceOf, got {other:?}"),
        }
    }
}
