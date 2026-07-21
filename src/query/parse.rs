//! OQL parser for the supported subset: a logos-derived lexer feeding a chumsky
//! combinator parser, with ariadne caret diagnostics for error rendering.
//!
//! Public entry points:
//!   - [`parse`] — production parse returning a compact single-line
//!     [`QueryError`] (`unexpected <found> at <line>:<col>`) for programmatic
//!     callers and tests.
//!   - [`parse_or_report`] — same parse but on failure returns a rendered
//!     ariadne diagnostic (caret + red underline) for CLI/REPL display.

use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::input::{Stream, ValueInput};
use chumsky::prelude::*;
use logos::Logos;

use crate::query::QueryError;
use crate::query::ast::{
    AggFunc, Attr, ClassSpec, CompareOp, FromSource, OrderBy, Predicate, Query, SelectItem,
    SortDir, Value,
};

/// OQL token kinds, lexed directly by logos.
///   - identifiers may contain `.`, `$`, and a trailing/embedded `*` glob
///   - `@attr` stores the name without the leading `@`
///   - strings are double-quoted, stored without quotes
///   - a bare `*` (not part of an ident) is `Star`
///
/// Derives `Debug, Clone, PartialEq` — chumsky and the tests rely on them.
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\r\n]+")]
pub enum Token {
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token(",")]
    Comma,
    #[token("=")]
    Eq,
    #[token("!=")]
    Ne,
    #[token("<=")]
    Le,
    #[token("<")]
    Lt,
    #[token(">=")]
    Ge,
    #[token(">")]
    Gt,
    #[token("*")]
    Star,

    // @attribute — capture the name after '@' (must be non-empty).
    #[regex(r"@[A-Za-z_][A-Za-z0-9_.$]*", |lex| lex.slice()[1..].to_string())]
    At(String),

    // double-quoted string — capture inner text (no escapes).
    #[regex(r#""[^"]*""#, |lex| { let s = lex.slice(); s[1..s.len()-1].to_string() })]
    Str(String),

    // float before int so "1.5" isn't split; optional leading '-'.
    #[regex(r"-?[0-9]+\.[0-9]*", |lex| lex.slice().parse::<f64>().ok())]
    Float(f64),
    #[regex(r"-?[0-9]+", |lex| lex.slice().parse::<i64>().ok())]
    Int(i64),

    // identifier / keyword / dotted class or field name, optional embedded '*'
    // glob, optional trailing '[]' pairs so array classes (e.g. `char[]`,
    // `java.lang.String[]`) are nameable in the FROM clause.
    #[regex(r"[A-Za-z_][A-Za-z0-9_.$*]*(\[\])*", |lex| lex.slice().to_string())]
    Ident(String),
}

/// Tokenize with byte-span tracking, producing the `(Token, SimpleSpan)` stream
/// consumed by the chumsky parser. On an unrecognized byte the error carries the
/// offending offset and slice.
pub fn tokenize_spanned(src: &str) -> Result<Vec<(Token, SimpleSpan)>, String> {
    let mut out = Vec::new();
    let mut lex = Token::lexer(src);
    while let Some(res) = lex.next() {
        let span = lex.span();
        match res {
            Ok(tok) => out.push((tok, (span.start..span.end).into())),
            Err(()) => {
                return Err(format!(
                    "unexpected character(s) at offset {}: {:?}",
                    span.start,
                    &src[span.clone()]
                ));
            }
        }
    }
    Ok(out)
}

fn ident_ci<'a, I>(kw: &'static str) -> impl Parser<'a, I, String, extra::Err<Rich<'a, Token>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(s) if s.eq_ignore_ascii_case(kw) => s }.labelled(kw)
}

fn any_ident<'a, I>() -> impl Parser<'a, I, String, extra::Err<Rich<'a, Token>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(s) => s }
}

/// Parses a `name ( ident )` dominator function form, yielding the single alias
/// identifier. A missing/malformed argument (e.g. `dominators()`) produces the
/// actionable `"<name>(x) requires a single alias argument, e.g. <name>(s)"`
/// error the callers assert on.
fn dom_fn<'a, I>(name: &'static str) -> impl Parser<'a, I, String, extra::Err<Rich<'a, Token>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    ident_ci(name)
        .ignore_then(just(Token::LParen))
        .ignore_then(any_ident().or_not())
        .then_ignore(just(Token::RParen))
        .validate(move |arg, e, emitter| match arg {
            Some(a) => a,
            None => {
                emitter.emit(Rich::custom(
                    e.span(),
                    format!("{name}(x) requires a single alias argument, e.g. {name}(s)"),
                ));
                String::new()
            }
        })
}

fn parser<'a, I>() -> impl Parser<'a, I, Query, extra::Err<Rich<'a, Token>>>
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // attribute: @built-in | classof(x) | bare field
    let attr = select! {
        Token::At(name) => name,
    }
    .try_map(|name, span| match name.as_str() {
        "objectId" => Ok(Attr::ObjectId),
        "objectAddress" => Ok(Attr::ObjectAddress),
        "usedHeapSize" => Ok(Attr::UsedHeapSize),
        "retainedHeapSize" | "retainedHeap" => Ok(Attr::RetainedHeapSize),
        "displayName" => Ok(Attr::DisplayName),
        "length" => Ok(Attr::Length),
        other => Err(Rich::custom(span, format!("unknown @attribute: @{other}"))),
    })
    .or(ident_ci("classof")
        .ignore_then(just(Token::LParen))
        .ignore_then(any_ident())
        .then_ignore(just(Token::RParen))
        .map(|_| Attr::ClassOf))
    .or(dom_fn("dominators").map(Attr::Dominators))
    .or(dom_fn("dominatorof").map(Attr::DominatorOf))
    .or(any_ident().map(Attr::Field))
    .labelled("attribute");

    // select item: AGG(item) | * | attr
    let select_item = recursive(|item| {
        let agg = select! {
            Token::Ident(s) if agg_func(&s).is_some() => agg_func(&s).unwrap(),
        }
        .then_ignore(just(Token::LParen))
        .then(item.clone())
        .then_ignore(just(Token::RParen))
        .map(|(func, arg): (AggFunc, SelectItem)| SelectItem::Aggregate { func, arg: Box::new(arg) });

        let star = just(Token::Star).map(|_| SelectItem::Star);

        agg.or(star).or(attr.clone().map(SelectItem::Attr))
    });

    let select_list = select_item
        .separated_by(just(Token::Comma))
        .at_least(1)
        .collect::<Vec<_>>();

    // value literal
    let value = select! {
        Token::Int(n) => Value::Int(n),
        Token::Float(f) => Value::Float(f),
        Token::Str(s) => Value::Str(s),
        Token::Ident(s) if s.eq_ignore_ascii_case("true") => Value::Bool(true),
        Token::Ident(s) if s.eq_ignore_ascii_case("false") => Value::Bool(false),
        Token::Ident(s) if s.eq_ignore_ascii_case("null") => Value::Null,
    }
    .labelled("literal value");

    let op = select! {
        Token::Eq => CompareOp::Eq,
        Token::Ne => CompareOp::Ne,
        Token::Lt => CompareOp::Lt,
        Token::Le => CompareOp::Le,
        Token::Gt => CompareOp::Gt,
        Token::Ge => CompareOp::Ge,
    }
    .labelled("comparison operator");

    // predicate grammar: OR < AND < NOT < primary
    let predicate = recursive(|pred| {
        let paren = just(Token::LParen)
            .ignore_then(pred.clone())
            .then_ignore(just(Token::RParen));

        let instanceof = attr
            .clone()
            .then_ignore(ident_ci("INSTANCEOF"))
            .then(any_ident())
            .map(|(_lhs, cname)| Predicate::InstanceOf(cname));

        let compare = attr
            .clone()
            .then(op)
            .then(value)
            .map(|((lhs, op), rhs)| Predicate::Compare { lhs, op, rhs });

        let primary = paren.or(instanceof).or(compare);

        let not = recursive(|not| {
            ident_ci("NOT")
                .ignore_then(not)
                .map(|p| Predicate::Not(Box::new(p)))
                .or(primary)
        });

        let and = not.clone().foldl(
            ident_ci("AND").ignore_then(not).repeated(),
            |l, r| Predicate::And(Box::new(l), Box::new(r)),
        );

        and.clone().foldl(
            ident_ci("OR").ignore_then(and).repeated(),
            |l, r| Predicate::Or(Box::new(l), Box::new(r)),
        )
    });

    // Optional `AS RETAINED SET` select modifier. `AS RETAINED` without a
    // trailing `SET` is a hard, actionable error rather than a silent miss.
    let retained_set = ident_ci("AS")
        .ignore_then(ident_ci("RETAINED"))
        .ignore_then(
            ident_ci("SET").to(true).or(any().or_not().validate(|_, e, emitter| {
                emitter.emit(Rich::custom(
                    e.span(),
                    "expected SET after 'AS RETAINED' (usage: SELECT <expr> AS RETAINED SET FROM ...)",
                ));
                true
            })),
        )
        .or_not()
        .map(|r| r.unwrap_or(false));

    let base_query = recursive(|base_query| {
        // FROM target: a parenthesized subquery `( <base_query> )`, or a class
        // pattern. INSTANCEOF applies only to the class form. UNION is not
        // reachable inside the parens (base_query has no UNION tail), so
        // `UNION` inside a subquery fails to parse — the intended rejection.
        let from_subquery = just(Token::LParen)
            .ignore_then(base_query)
            .then_ignore(just(Token::RParen))
            .map(|inner: Query| FromSource::Subquery(Box::new(inner)));
        let from_class = ident_ci("INSTANCEOF")
            .or_not()
            .map(|i| i.is_some())
            .then(any_ident())
            .map(|(instanceof, class_name)| {
                FromSource::Class(ClassSpec { instanceof, class_name })
            });
        let from_source = from_subquery.or(from_class);

        ident_ci("SELECT")
            .ignore_then(ident_ci("DISTINCT").or_not().map(|d| d.is_some()))
            .then(select_list.clone())
            .then(retained_set.clone())
            .then_ignore(ident_ci("FROM"))
            .then(from_source)
            .then(any_ident().and_is(reserved_ident().not()).or_not())
            .then(ident_ci("WHERE").ignore_then(predicate.clone()).or_not())
            .then(
                ident_ci("ORDER")
                    .ignore_then(ident_ci("BY"))
                    .ignore_then(attr.clone())
                    .then(
                        ident_ci("ASC")
                            .to(SortDir::Asc)
                            .or(ident_ci("DESC").to(SortDir::Desc))
                            .or_not()
                            .map(|d| d.unwrap_or(SortDir::Asc)),
                    )
                    .map(|(key, dir)| OrderBy { key, dir })
                    .or_not(),
            )
            .then(
                ident_ci("LIMIT")
                    .ignore_then(select! { Token::Int(n) if n >= 0 => n as u64 }.labelled("LIMIT count"))
                    .or_not(),
            )
            .map(
                |(((((((distinct, select), retained_set), from), alias), where_), order_by), limit)| {
                    Query {
                        distinct,
                        select,
                        retained_set,
                        from,
                        alias,
                        where_,
                        order_by,
                        limit,
                        union_branches: Vec::new(),
                    }
                },
            )
    });

    // Top level: a base query, then a flat `UNION`-separated tail folded into the
    // head's `union_branches`. Tail branches keep empty `union_branches` (the
    // list is flat, left-associative concatenation with UNION ALL semantics).
    base_query
        .clone()
        .then(
            ident_ci("UNION")
                .ignore_then(base_query)
                .repeated()
                .collect::<Vec<_>>(),
        )
        .then_ignore(end())
        .map(|(mut head, tail): (Query, Vec<Query>)| {
            head.union_branches = tail;
            head
        })
}

fn reserved_ident<'a, I>() -> impl Parser<'a, I, String, extra::Err<Rich<'a, Token>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(s) if is_reserved(&s) => s }
}

/// Clause/grammar keywords that open or structure a query but are not reserved
/// words in predicate position. `classof` is the pseudo-function attribute form.
pub const KEYWORDS: &[&str] = &["SELECT", "DISTINCT", "FROM", "classof"];

/// Words reserved in predicate/clause position (`is_reserved`'s source set).
pub const RESERVED: &[&str] = &[
    "WHERE", "LIMIT", "UNION", "AND", "OR", "NOT", "INSTANCEOF", "ORDER", "BY", "ASC", "DESC",
];

/// Aggregate function names (`agg_func`'s source set), upper-cased.
pub const AGG_FUNCS: &[&str] = &["COUNT", "SUM", "MIN", "MAX", "AVG"];

/// `@`-prefixed built-in attribute names (matching the `attr` parser's arms),
/// including the leading `@` so they can be offered as completions directly.
pub const ATTRIBUTES: &[&str] = &[
    "@objectId",
    "@objectAddress",
    "@usedHeapSize",
    "@retainedHeapSize",
    "@displayName",
    "@length",
];

/// The full set of completion candidates offered by the REPL, sourced from the
/// same const slices the parser matches against — the single point of truth for
/// keyword knowledge, so completions can never drift from the grammar.
pub fn completion_words() -> Vec<&'static str> {
    KEYWORDS
        .iter()
        .chain(RESERVED.iter())
        .chain(AGG_FUNCS.iter())
        .chain(ATTRIBUTES.iter())
        .copied()
        .collect()
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
    RESERVED.iter().any(|k| s.eq_ignore_ascii_case(k))
}

/// 1-based (line, column) of a byte offset within `src`, for compact error
/// messages. A byte offset at or past `src.len()` reports the position just
/// past the last character.
fn line_col(src: &str, byte_offset: usize) -> (usize, usize) {
    let capped = byte_offset.min(src.len());
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in src.char_indices() {
        if i >= capped {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Compact single-line message for one chumsky error. Custom (`Rich::custom`)
/// reasons — e.g. `dominators(x) requires ...` — are surfaced verbatim with a
/// trailing `at <line>:<col>`; positional errors keep the `unexpected <found>`
/// form. Shared by [`parse_internal`] and [`parse_or_report`] so custom
/// diagnostics never get flattened into a generic "unexpected" message.
fn compact_error(src: &str, e: &Rich<'_, Token>) -> String {
    let span = *e.span();
    let (line, col) = line_col(src, span.start);
    match e.reason() {
        chumsky::error::RichReason::Custom(msg) => format!("{msg} at {line}:{col}"),
        _ => {
            let found = e
                .found()
                .map(|t| format!("{t:?}"))
                .unwrap_or_else(|| "end of input".to_string());
            format!("unexpected {found} at {line}:{col}")
        }
    }
}

/// Tokenize (logos) → chumsky parse, returning a compact single-line message
/// `unexpected <found> at <line>:<col>` (or the tokenizer's offset message) on
/// failure. Backs [`parse`]; [`parse_or_report`] renders errors differently.
fn parse_internal(src: &str) -> Result<Query, String> {
    let toks = tokenize_spanned(src)?;
    let eoi: SimpleSpan = (src.len()..src.len()).into();
    let stream = Stream::from_iter(toks).map(eoi, |(t, s): (Token, SimpleSpan)| (t, s));
    parser().parse(stream).into_result().map_err(|errs| {
        errs.iter()
            .map(|e| compact_error(src, e))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

/// Production parse: chumsky over logos tokens. Returns a [`QueryError`] whose
/// message is a compact single-line description (no caret art) for programmatic
/// callers and tests. Production CLI/REPL paths use [`parse_or_report`] instead
/// for caret diagnostics, so this is currently reached only from tests.
#[allow(dead_code)]
pub fn parse(src: &str) -> Result<Query, QueryError> {
    parse_internal(src).map_err(QueryError)
}

/// Parse, returning either the [`Query`] or a rendered ariadne diagnostic string
/// (caret + red underline) for CLI/REPL display.
pub fn parse_or_report(src: &str) -> Result<Query, String> {
    // Tokenizer errors have no chumsky span to underline; surface the message.
    let toks = match tokenize_spanned(src) {
        Ok(t) => t,
        Err(e) => return Err(format!("tokenize error: {e}")),
    };
    let eoi: SimpleSpan = (src.len()..src.len()).into();
    let stream = Stream::from_iter(toks).map(eoi, |(t, s): (Token, SimpleSpan)| (t, s));
    match parser().parse(stream).into_result() {
        Ok(q) => Ok(q),
        Err(errs) => {
            let mut buf = Vec::new();
            for e in &errs {
                let span = *e.span();
                let msg = match e.reason() {
                    chumsky::error::RichReason::Custom(m) => m.clone(),
                    _ => {
                        let found = e
                            .found()
                            .map(|t| format!("{t:?}"))
                            .unwrap_or_else(|| "end of input".to_string());
                        format!("unexpected {found}")
                    }
                };
                let mut out = Vec::new();
                Report::build(ReportKind::Error, ("query", span.into_range()))
                    .with_message(&msg)
                    .with_label(
                        Label::new(("query", span.into_range()))
                            .with_message(&msg)
                            .with_color(Color::Red),
                    )
                    .finish()
                    .write(("query", Source::from(src)), &mut out)
                    .ok();
                buf.push(String::from_utf8_lossy(&out).into_owned());
            }
            Err(buf.join("\n"))
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::*;

    // ---------- helpers ----------

    fn toks(src: &str) -> Vec<Token> {
        tokenize_spanned(src)
            .unwrap_or_else(|e| panic!("tokenize failed for {src:?}: {e}"))
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }
    fn id(s: &str) -> Token {
        Token::Ident(s.into())
    }
    // AST builders
    fn field(s: &str) -> Attr {
        Attr::Field(s.into())
    }
    fn cmp(lhs: Attr, op: CompareOp, rhs: Value) -> Predicate {
        Predicate::Compare { lhs, op, rhs }
    }
    fn and(a: Predicate, b: Predicate) -> Predicate {
        Predicate::And(Box::new(a), Box::new(b))
    }
    fn or(a: Predicate, b: Predicate) -> Predicate {
        Predicate::Or(Box::new(a), Box::new(b))
    }
    fn not(a: Predicate) -> Predicate {
        Predicate::Not(Box::new(a))
    }
    fn q(
        distinct: bool,
        select: Vec<SelectItem>,
        instanceof: bool,
        class_name: &str,
        alias: Option<&str>,
        where_: Option<Predicate>,
        limit: Option<u64>,
    ) -> Query {
        Query {
            distinct,
            select,
            retained_set: false,
            from: FromSource::Class(ClassSpec { instanceof, class_name: class_name.into() }),
            alias: alias.map(|s| s.into()),
            where_,
            order_by: None,
            limit,
            union_branches: Vec::new(),
        }
    }
    fn star() -> Vec<SelectItem> {
        vec![SelectItem::Star]
    }
    fn attr_sel(a: Attr) -> SelectItem {
        SelectItem::Attr(a)
    }
    fn agg(func: AggFunc, arg: SelectItem) -> SelectItem {
        SelectItem::Aggregate { func, arg: Box::new(arg) }
    }

    // ============================================================
    // Group 1 — token-stream cases (input → expected Vec<Token>)
    // 15 cases exercising each token kind + lexer edge cases.
    // ============================================================

    #[test]
    fn token_stream_cases() {
        let cases: Vec<(&str, Vec<Token>)> = vec![
            // 1: keywords + star + dotted ident + alias
            (
                "SELECT * FROM java.lang.String s",
                vec![id("SELECT"), Token::Star, id("FROM"), id("java.lang.String"), id("s")],
            ),
            // 2: @attr, comparison, int, AND, ident, eq, string
            (
                "WHERE @usedHeapSize > 100 AND name = \"foo\"",
                vec![
                    id("WHERE"),
                    Token::At("usedHeapSize".into()),
                    Token::Gt,
                    Token::Int(100),
                    id("AND"),
                    id("name"),
                    Token::Eq,
                    Token::Str("foo".into()),
                ],
            ),
            // 3: all comparison operators
            (
                "= != < <= > >=",
                vec![Token::Eq, Token::Ne, Token::Lt, Token::Le, Token::Gt, Token::Ge],
            ),
            // 4: parens + comma
            ("( , )", vec![Token::LParen, Token::Comma, Token::RParen]),
            // 5: negative float
            ("-3.5", vec![Token::Float(-3.5)]),
            // 6: negative int
            ("-42", vec![Token::Int(-42)]),
            // 7: float before int (no split of 1.5)
            ("1.5", vec![Token::Float(1.5)]),
            // 8: trailing-dot float (regex allows empty fraction)
            ("7.", vec![Token::Float(7.0)]),
            // 9: glob class name with '*'
            ("com.acme.*", vec![id("com.acme.*")]),
            // 10: embedded glob
            ("com.a*b.C", vec![id("com.a*b.C")]),
            // 11: inner-class '$'
            ("Outer$Inner", vec![id("Outer$Inner")]),
            // 12: underscore leading ident
            ("_hidden", vec![id("_hidden")]),
            // 13: empty string literal
            ("\"\"", vec![Token::Str("".into())]),
            // 14: string with spaces preserved
            ("\"a b c\"", vec![Token::Str("a b c".into())]),
            // 15: @attr with dot/dollar in name
            ("@a.b$c", vec![Token::At("a.b$c".into())]),
            // 16: bare star vs glob-ident disambiguation
            ("* x*", vec![Token::Star, id("x*")]),
            // 17: whitespace (tabs/newlines) skipped
            ("SELECT\t*\nFROM\rC", vec![id("SELECT"), Token::Star, id("FROM"), id("C")]),
            // 18: primitive array class name (trailing '[]')
            ("char[]", vec![id("char[]")]),
            // 19: object array class name (dotted + trailing '[]')
            ("java.lang.String[]", vec![id("java.lang.String[]")]),
            // 20: multi-dimensional array class name (repeated '[]')
            ("int[][]", vec![id("int[][]")]),
        ];
        for (src, expected) in cases {
            assert_eq!(toks(src), expected, "token stream mismatch for {src:?}");
        }
    }

    // ============================================================
    // Group 2 — full-AST cases (input → expected Query)
    // 22 cases covering select/from/alias/where/limit combinations.
    // ============================================================

    #[test]
    fn ast_cases() {
        let cases: Vec<(&str, Query)> = vec![
            // 1: bare star + alias
            (
                "SELECT * FROM java.lang.String s",
                q(false, star(), false, "java.lang.String", Some("s"), None, None),
            ),
            // 2: star, no alias
            ("SELECT * FROM C", q(false, star(), false, "C", None, None, None)),
            // 3: DISTINCT
            (
                "SELECT DISTINCT name FROM C",
                q(false, vec![attr_sel(field("name"))], false, "C", None, None, None)
                    .tap_distinct(),
            ),
            // 4: INSTANCEOF in FROM
            (
                "SELECT * FROM INSTANCEOF java.util.List",
                q(false, star(), true, "java.util.List", None, None, None),
            ),
            // 5: LIMIT
            (
                "SELECT * FROM C LIMIT 5",
                q(false, star(), false, "C", None, None, Some(5)),
            ),
            // 6: LIMIT 0 (zero allowed)
            (
                "SELECT * FROM C LIMIT 0",
                q(false, star(), false, "C", None, None, Some(0)),
            ),
            // 7: multiple select items
            (
                "SELECT @objectId, name, @usedHeapSize FROM C",
                q(
                    false,
                    vec![
                        attr_sel(Attr::ObjectId),
                        attr_sel(field("name")),
                        attr_sel(Attr::UsedHeapSize),
                    ],
                    false,
                    "C",
                    None,
                    None,
                    None,
                ),
            ),
            // 8: all built-in attrs
            (
                "SELECT @objectId, @objectAddress, @usedHeapSize, @displayName, @length FROM C",
                q(
                    false,
                    vec![
                        attr_sel(Attr::ObjectId),
                        attr_sel(Attr::ObjectAddress),
                        attr_sel(Attr::UsedHeapSize),
                        attr_sel(Attr::DisplayName),
                        attr_sel(Attr::Length),
                    ],
                    false,
                    "C",
                    None,
                    None,
                    None,
                ),
            ),
            // 9: classof(alias)
            (
                "SELECT classof(s) FROM java.lang.String s",
                q(false, vec![attr_sel(Attr::ClassOf)], false, "java.lang.String", Some("s"), None, None),
            ),
            // 10: COUNT(*)
            (
                "SELECT COUNT(*) FROM C",
                q(false, vec![agg(AggFunc::Count, SelectItem::Star)], false, "C", None, None, None),
            ),
            // 11: SUM(@usedHeapSize)
            (
                "SELECT SUM(@usedHeapSize) FROM C",
                q(
                    false,
                    vec![agg(AggFunc::Sum, attr_sel(Attr::UsedHeapSize))],
                    false,
                    "C",
                    None,
                    None,
                    None,
                ),
            ),
            // 12: MIN/MAX/AVG mixed
            (
                "SELECT MIN(x), MAX(x), AVG(x) FROM C",
                q(
                    false,
                    vec![
                        agg(AggFunc::Min, attr_sel(field("x"))),
                        agg(AggFunc::Max, attr_sel(field("x"))),
                        agg(AggFunc::Avg, attr_sel(field("x"))),
                    ],
                    false,
                    "C",
                    None,
                    None,
                    None,
                ),
            ),
            // 13: simple WHERE compare int
            (
                "SELECT * FROM C WHERE hash > 0",
                q(false, star(), false, "C", None, Some(cmp(field("hash"), CompareOp::Gt, Value::Int(0))), None),
            ),
            // 14: WHERE compare string
            (
                "SELECT * FROM C WHERE name = \"main\"",
                q(false, star(), false, "C", None, Some(cmp(field("name"), CompareOp::Eq, Value::Str("main".into()))), None),
            ),
            // 15: WHERE compare float
            (
                "SELECT * FROM C WHERE ratio <= 1.5",
                q(false, star(), false, "C", None, Some(cmp(field("ratio"), CompareOp::Le, Value::Float(1.5))), None),
            ),
            // 16: WHERE bool true
            (
                "SELECT * FROM C WHERE flag = true",
                q(false, star(), false, "C", None, Some(cmp(field("flag"), CompareOp::Eq, Value::Bool(true))), None),
            ),
            // 17: WHERE bool false + null (case-insensitive keywords)
            (
                "SELECT * FROM C WHERE a = FALSE AND b != NULL",
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(and(
                        cmp(field("a"), CompareOp::Eq, Value::Bool(false)),
                        cmp(field("b"), CompareOp::Ne, Value::Null),
                    )),
                    None,
                ),
            ),
            // 18: precedence NOT/AND/OR
            (
                "SELECT * FROM C WHERE NOT a = 1 OR b = 2 AND c = 3",
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(or(
                        not(cmp(field("a"), CompareOp::Eq, Value::Int(1))),
                        and(
                            cmp(field("b"), CompareOp::Eq, Value::Int(2)),
                            cmp(field("c"), CompareOp::Eq, Value::Int(3)),
                        ),
                    )),
                    None,
                ),
            ),
            // 19: parenthesized predicate overrides precedence
            (
                "SELECT * FROM C WHERE (a = 1 OR b = 2) AND c = 3",
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(and(
                        or(
                            cmp(field("a"), CompareOp::Eq, Value::Int(1)),
                            cmp(field("b"), CompareOp::Eq, Value::Int(2)),
                        ),
                        cmp(field("c"), CompareOp::Eq, Value::Int(3)),
                    )),
                    None,
                ),
            ),
            // 20: predicate-level INSTANCEOF
            (
                "SELECT * FROM C WHERE s INSTANCEOF java.lang.String",
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(Predicate::InstanceOf("java.lang.String".into())),
                    None,
                ),
            ),
            // 21: double negation
            (
                "SELECT * FROM C WHERE NOT NOT a = 1",
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(not(not(cmp(field("a"), CompareOp::Eq, Value::Int(1))))),
                    None,
                ),
            ),
            // 22: everything at once — DISTINCT + INSTANCEOF + alias + WHERE + LIMIT
            (
                "SELECT DISTINCT @objectId, name FROM INSTANCEOF java.lang.Thread t \
                 WHERE @usedHeapSize >= 100 AND name != \"main\" LIMIT 5",
                q(
                    true,
                    vec![attr_sel(Attr::ObjectId), attr_sel(field("name"))],
                    true,
                    "java.lang.Thread",
                    Some("t"),
                    Some(and(
                        cmp(Attr::UsedHeapSize, CompareOp::Ge, Value::Int(100)),
                        cmp(field("name"), CompareOp::Ne, Value::Str("main".into())),
                    )),
                    Some(5),
                ),
            ),
            // 23: negative int literal in WHERE
            (
                "SELECT * FROM C WHERE delta = -7",
                q(false, star(), false, "C", None, Some(cmp(field("delta"), CompareOp::Eq, Value::Int(-7))), None),
            ),
            // 24: nested aggregate arg (AGG over attr)
            (
                "SELECT COUNT(name) FROM C",
                q(false, vec![agg(AggFunc::Count, attr_sel(field("name")))], false, "C", None, None, None),
            ),
        ];
        for (src, expected) in cases {
            let got = parse(src).unwrap_or_else(|e| panic!("parse failed for {src:?}: {}", e.0));
            assert_eq!(got, expected, "AST mismatch for {src:?}");
        }
    }

    // ============================================================
    // Group 3 — error cases: compact single-line message w/ line:col
    // 12 cases of malformed input.
    // ============================================================

    #[test]
    fn error_cases() {
        // Each entry: (src, substring the message must contain)
        let cases: Vec<(&str, &str)> = vec![
            ("", "unexpected"),                                   // 1 empty
            ("SELECT", "unexpected"),                             // 2 select only
            ("SELECT *", "unexpected"),                           // 3 missing FROM
            ("SELECT * FROM", "unexpected"),                      // 4 FROM no class
            ("SELECT * FROM C bogus extra", "unexpected"),        // 5 trailing garbage
            ("SELECT @bogus FROM C", "bogus"),                   // 6 unknown builtin attr
            ("SELECT * FROM C WHERE hash >", "unexpected"),       // 7 dangling operator
            ("SELECT * FROM C WHERE hash", "unexpected"),         // 8 missing operator+rhs
            ("SELECT * FROM C LIMIT abc", "unexpected"),          // 9 non-int limit
            ("SELECT * FROM C LIMIT -1", "unexpected"),           // 10 negative limit rejected
            ("SELECT COUNT * FROM C", "unexpected"),              // 11 agg missing paren
            ("SELECT * FROM C WHERE (a = 1", "unexpected"),       // 12 unbalanced paren
            ("SELECT , FROM C", "unexpected"),                    // 13 empty select item
            ("SELECT * FROM C WHERE a = ", "unexpected"),         // 14 missing rhs value
            ("SELECT * FROM C WHERE a == 1", "unexpected"),       // 15 bad operator ==
        ];
        for (src, needle) in cases {
            let err = parse(src)
                .err()
                .unwrap_or_else(|| panic!("expected parse error for {src:?}"))
                .0;
            assert!(!err.is_empty(), "empty error for {src:?}");
            assert!(!err.contains('\n'), "expected single-line error for {src:?}, got: {err}");
            assert!(
                err.contains(needle),
                "error for {src:?} should contain {needle:?}, got: {err}"
            );
            // Compact messages carry a line:col (except tokenizer-offset errors).
            assert!(
                err.contains(':') || err.contains("offset"),
                "error for {src:?} should carry a location, got: {err}"
            );
        }
    }

    #[test]
    fn tokenizer_error_cases() {
        // Bytes logos can't lex: report an offset. `#` and `&` are not in any regex.
        for src in ["SELECT * FROM C WHERE a = #", "a & b", "SELECT ~ FROM C"] {
            let err = tokenize_spanned(src)
                .err()
                .unwrap_or_else(|| panic!("expected tokenize error for {src:?}"));
            assert!(err.contains("offset"), "got: {err}");
        }
        // Unterminated string: the lone `"` cannot start a valid string token.
        let err = tokenize_spanned("name = \"foo").unwrap_err();
        assert!(err.contains("offset"), "got: {err}");
    }

    // ---------- targeted unit tests ----------

    #[test]
    fn union_two_branches_parses() {
        let q = parse("SELECT * FROM java.lang.String UNION SELECT * FROM java.lang.Integer").unwrap();
        assert_eq!(q.union_branches.len(), 1);
        assert_eq!(q.union_branches[0].from.class_name(), "java.lang.Integer");
        assert!(q.union_branches[0].union_branches.is_empty(), "branches must be flat, not nested");
    }
    #[test]
    fn union_three_branches_flat() {
        let q = parse("SELECT * FROM A UNION SELECT * FROM B UNION SELECT * FROM C").unwrap();
        assert_eq!(q.union_branches.len(), 2);
        assert_eq!(q.union_branches[0].from.class_name(), "B");
        assert_eq!(q.union_branches[1].from.class_name(), "C");
        assert!(q.union_branches.iter().all(|b| b.union_branches.is_empty()));
    }
    #[test]
    fn no_union_leaves_branches_empty() {
        assert!(parse("SELECT * FROM C").unwrap().union_branches.is_empty());
    }

    #[test]
    fn from_subquery_parses() {
        let q = parse("SELECT * FROM (SELECT * FROM java.lang.String) x").unwrap();
        match &q.from {
            FromSource::Subquery(inner) => {
                assert_eq!(inner.from.class_name(), "java.lang.String");
                assert!(inner.union_branches.is_empty());
            }
            other => panic!("expected subquery FROM, got {other:?}"),
        }
        assert_eq!(q.alias.as_deref(), Some("x"));
    }

    #[test]
    fn from_class_still_parses_after_migration() {
        let q = parse("SELECT * FROM java.lang.String s").unwrap();
        assert_eq!(q.from.class_name(), "java.lang.String");
        assert!(!q.from.instanceof());
        assert!(q.from.as_subquery().is_none());
        assert_eq!(q.alias.as_deref(), Some("s"));
    }

    #[test]
    fn from_instanceof_class_sets_flag() {
        let q = parse("SELECT * FROM INSTANCEOF java.util.List").unwrap();
        assert!(q.from.instanceof(), "INSTANCEOF flag must survive migration");
        assert_eq!(q.from.class_name(), "java.util.List");
    }

    #[test]
    fn union_inside_subquery_is_rejected() {
        // UNION is unreachable inside the parenthesized base_query, so the inner
        // UNION fails to parse (located error) rather than silently nesting.
        let err = parse("SELECT * FROM (SELECT * FROM A UNION SELECT * FROM B) x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unexpected"), "expected a located parse error, got: {err}");
        assert!(err.contains(':'), "error should carry a line:col location, got: {err}");
    }

    #[test]
    fn parse_dominators_attr() {
        let q = parse("SELECT dominators(s) FROM java.lang.String s").unwrap();
        assert_eq!(q.select.len(), 1);
        match &q.select[0] {
            SelectItem::Attr(Attr::Dominators(v)) => assert_eq!(v, "s"),
            other => panic!("expected Attr::Dominators, got {other:?}"),
        }
    }
    #[test]
    fn parse_dominatorof_attr() {
        let q = parse("SELECT dominatorof(s) FROM java.lang.String s").unwrap();
        match &q.select[0] {
            SelectItem::Attr(Attr::DominatorOf(v)) => assert_eq!(v, "s"),
            other => panic!("expected Attr::DominatorOf, got {other:?}"),
        }
    }
    #[test]
    fn parse_dominators_requires_arg() {
        let err = parse("SELECT dominators() FROM java.lang.String s").unwrap_err();
        assert!(err.to_string().contains("dominators(x) requires"), "unexpected error: {err}");
    }
    #[test]
    fn parse_dominatorof_requires_arg() {
        let err = parse("SELECT dominatorof() FROM java.lang.String s").unwrap_err();
        assert!(err.to_string().contains("dominatorof(x) requires"), "unexpected error: {err}");
    }
    #[test]
    fn dominators_in_select_list_with_other_items() {
        // A dominator attr coexists with a plain attr in the projection list.
        let q = parse("SELECT @objectId, dominators(s) FROM java.lang.String s").unwrap();
        assert_eq!(q.select.len(), 2);
        assert!(matches!(&q.select[1], SelectItem::Attr(Attr::Dominators(v)) if v == "s"));
    }
    #[test]
    fn dominatorof_report_error_names_function() {
        // The caret-rendered report also carries the actionable custom message.
        let rep = parse_or_report("SELECT dominatorof() FROM C").unwrap_err();
        assert!(rep.contains("dominatorof(x) requires"), "report missing message: {rep}");
    }

    #[test]
    fn parse_as_retained_set() {
        let q = parse("SELECT s AS RETAINED SET FROM java.lang.String s").unwrap();
        assert!(q.retained_set);
        assert_eq!(q.select.len(), 1);
    }
    #[test]
    fn parse_no_retained_set_default_false() {
        assert!(!parse("SELECT s FROM java.lang.String s").unwrap().retained_set);
    }
    #[test]
    fn parse_as_retained_missing_set() {
        let err = parse("SELECT s AS RETAINED FROM java.lang.String s").unwrap_err();
        assert!(err.to_string().contains("expected SET after 'AS RETAINED'"), "unexpected: {err}");
    }
    #[test]
    fn parse_as_retained_set_with_where_and_limit() {
        // The modifier composes with the rest of the clause chain.
        let q = parse(
            "SELECT s AS RETAINED SET FROM java.lang.String s WHERE @retainedHeapSize > 0 LIMIT 5",
        )
        .unwrap();
        assert!(q.retained_set);
        assert!(q.where_.is_some());
        assert_eq!(q.limit, Some(5));
    }
    #[test]
    fn parse_as_retained_case_insensitive() {
        assert!(parse("SELECT s as retained set FROM C s").unwrap().retained_set);
    }

    #[test]
    fn parses_retained_heap_size_attr() {
        let q = parse("SELECT @retainedHeapSize FROM C").unwrap();
        assert_eq!(q.select, vec![SelectItem::Attr(Attr::RetainedHeapSize)]);
    }
    fn retained_heap_alias_normalizes_to_retained_heap_size() {
        let q = parse("SELECT @retainedHeap FROM C").unwrap();
        assert_eq!(q.select, vec![SelectItem::Attr(Attr::RetainedHeapSize)]);
    }
    #[test]
    fn retained_heap_size_usable_in_where() {
        let q = parse("SELECT @objectId FROM C WHERE @retainedHeapSize > 1024").unwrap();
        assert!(q.where_.is_some());
    }

    #[test]
    fn parses_order_by_desc() {
        let q = parse("SELECT @objectId FROM C ORDER BY @retainedHeapSize DESC").unwrap();
        let ob = q.order_by.expect("ORDER BY parsed");
        assert_eq!(ob.key, Attr::RetainedHeapSize);
        assert_eq!(ob.dir, SortDir::Desc);
    }
    #[test]
    fn order_by_defaults_to_asc() {
        let q = parse("SELECT @objectId FROM C ORDER BY @usedHeapSize").unwrap();
        assert_eq!(q.order_by.unwrap().dir, SortDir::Asc);
    }
    #[test]
    fn order_by_before_limit() {
        let q = parse("SELECT @objectId FROM C ORDER BY @retainedHeapSize DESC LIMIT 10").unwrap();
        assert!(q.order_by.is_some());
        assert_eq!(q.limit, Some(10));
    }
    #[test]
    fn no_order_by_is_none() {
        let q = parse("SELECT @objectId FROM C").unwrap();
        assert!(q.order_by.is_none());
    }

    #[test]
    fn span_tracks_byte_offsets() {
        let lg = tokenize_spanned("@usedHeapSize").expect("logos tokenizes");
        assert_eq!(lg.len(), 1);
        let (tok, span) = &lg[0];
        assert_eq!(*tok, Token::At("usedHeapSize".into()));
        assert_eq!((span.start, span.end), (0, 13));
    }

    #[test]
    fn line_col_basic() {
        assert_eq!(line_col("abc", 0), (1, 1));
        assert_eq!(line_col("abc", 2), (1, 3));
        assert_eq!(line_col("ab\ncd", 3), (2, 1));
        assert_eq!(line_col("ab\ncd", 4), (2, 2));
        assert_eq!(line_col("abc", 99), (1, 4)); // clamps past end
    }

    #[test]
    fn report_contains_caret_marker() {
        let rep = parse_or_report("SELCT * FROM C").unwrap_err();
        assert!(rep.contains("query:1:"), "expected caret location, got:\n{rep}");
    }

    #[test]
    fn report_ok_on_valid_query() {
        assert!(parse_or_report("SELECT * FROM C").is_ok());
    }

    #[test]
    fn report_tokenizer_error_surfaced() {
        let rep = parse_or_report("SELECT * FROM C WHERE a = #").unwrap_err();
        assert!(rep.contains("tokenize error"), "got: {rep}");
    }

    // The canonical keyword slices must stay in sync with the parser's actual
    // matching logic — otherwise the REPL completer (which sources them via
    // `completion_words`) would drift from what the grammar accepts.

    #[test]
    fn agg_funcs_const_matches_parser() {
        for &f in AGG_FUNCS {
            assert!(agg_func(f).is_some(), "agg_func rejects declared AGG_FUNC {f:?}");
            // ...and the query actually parses as an aggregate.
            assert!(
                parse(&format!("SELECT {f}(*) FROM C")).is_ok(),
                "parser rejects aggregate {f:?}"
            );
        }
    }

    #[test]
    fn reserved_const_matches_parser() {
        for &r in RESERVED {
            assert!(is_reserved(r), "is_reserved rejects declared RESERVED {r:?}");
        }
    }

    #[test]
    fn attributes_const_all_parse() {
        for &a in ATTRIBUTES {
            // Each declared @attribute must parse as a SELECT column.
            assert!(
                parse(&format!("SELECT {a} FROM C")).is_ok(),
                "parser rejects declared attribute {a:?}"
            );
        }
    }

    #[test]
    fn completion_words_covers_all_sources() {
        let words = completion_words();
        for set in [KEYWORDS, RESERVED, AGG_FUNCS, ATTRIBUTES] {
            for &w in set {
                assert!(words.contains(&w), "completion_words missing {w:?}");
            }
        }
    }

    // small helper: fluent DISTINCT flip for the ast_cases table
    trait TapDistinct {
        fn tap_distinct(self) -> Self;
    }
    impl TapDistinct for Query {
        fn tap_distinct(mut self) -> Self {
            self.distinct = true;
            self
        }
    }
}
