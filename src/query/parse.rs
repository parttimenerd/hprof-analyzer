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
    AggFunc, ArithOp, Attr, ClassSpec, CompareOp, Expr, FromSource, OrderBy, PathOperand,
    Predicate, Query, RefRole, SelectItem, SortDir, UnaryOp, Value,
};

/// OQL token kinds, lexed directly by logos.
///   - identifiers may contain `.`, `$`, and a trailing/embedded `*` glob
///   - `@attr` stores the name without the leading `@`
///   - strings are double-quoted, stored without quotes
///   - a bare `*` (not part of an ident) is `Star`
///   - arithmetic operators `+`, `-`, `/`, `*` are dedicated tokens; unary
///     minus is handled at the grammar level (no leading `-` in number literals)
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
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("/")]
    Divide,

    // @attribute — capture the name after '@' (must be non-empty).
    #[regex(r"@[A-Za-z_][A-Za-z0-9_.$]*", |lex| lex.slice()[1..].to_string())]
    At(String),

    // double-quoted string — capture inner text (no escapes).
    #[regex(r#""[^"]*""#, |lex| { let s = lex.slice(); s[1..s.len()-1].to_string() })]
    Str(String),

    // Numeric + char literals. All int forms collapse to Int(i64); all float
    // forms to Float(f64) (our Value has no int/long/float/double/char split).
    // Order: float before int so "1.5" isn't split. Char and all int forms all
    // attach to Int. No leading '-': unary minus is a grammar operator.
    //
    // Float: `[digits].[digits]`, `[digits].`, dotless-with-suffix `[digits][fFdD]`,
    // and exponent forms; optional trailing f/F/d/D stripped before parse.
    // priority=4 beats Ident (default 2) so dotless 5F/5D lex as Float, not Ident
    #[regex(
        r"[0-9]+\.[0-9]*([eE][+-]?[0-9]+)?[fFdD]?|[0-9]+([eE][+-]?[0-9]+)[fFdD]?|[0-9]+[fFdD]",
        |lex| {
            let s = lex.slice();
            let core = s.trim_end_matches(['f', 'F', 'd', 'D']);
            core.parse::<f64>().ok()
        },
        priority = 4
    )]
    Float(f64),
    // Char: exactly one char between single quotes, no escapes (MAT CHARACTER_LITERAL).
    #[regex(r"'[^'\\\n\r]'", |lex| {
        let s = lex.slice();
        s[1..s.len() - 1].chars().next().map(|c| c as i64)
    })]
    // Hex: 0x… with optional L suffix. Parsed as u64 then reinterpreted as i64
    // (bit-preserving) so high-bit heap addresses (> i64::MAX) round-trip.
    #[regex(r"0[xX][0-9a-fA-F]+[lL]?", |lex| {
        let s = lex.slice().trim_end_matches(['l', 'L']);
        u64::from_str_radix(&s[2..], 16).ok().map(|v| v as i64)
    }, priority = 3)]
    // Octal: leading 0 followed by 1+ octal digits, optional L. (Lone `0` and
    // `08`/`09` fall through to the decimal arm — lenient MAT divergence.)
    // leading 0 is a valid octal digit; from_str_radix(_, 8) handles the full slice.
    // Parsed as u64 then bit-reinterpreted as i64 (see hex arm).
    #[regex(r"0[0-7]+[lL]?", |lex| {
        let s = lex.slice().trim_end_matches(['l', 'L']);
        u64::from_str_radix(s, 8).ok().map(|v| v as i64)
    }, priority = 3)]
    // Decimal int/long: digits with optional L. Also catches lone `0`, `08`, `09`.
    // Parsed as u64 then bit-reinterpreted as i64 (see hex arm).
    #[regex(r"[0-9]+[lL]?", |lex| {
        let s = lex.slice().trim_end_matches(['l', 'L']);
        s.parse::<u64>().ok().map(|v| v as i64)
    })]
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
                let slice = &src[span.clone()];
                if slice.starts_with('\'') {
                    return Err(format!(
                        "character literal must contain exactly one character: {slice:?} \
                         (single-quoted, no escapes)"
                    ));
                }
                if slice.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    return Err(format!(
                        "numeric literal out of range for a 64-bit integer (unsigned range, \
                         0..=0xffffffffffffffff): {slice:?}"
                    ));
                }
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

fn ident_ci<'a, I>(
    kw: &'static str,
) -> impl Parser<'a, I, String, extra::Err<Rich<'a, Token>>> + Clone
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
fn dom_fn<'a, I>(
    name: &'static str,
) -> impl Parser<'a, I, String, extra::Err<Rich<'a, Token>>> + Clone
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
    // attribute: [alias.]@built-in | classof(x) | bare field
    //
    // `@attr`, optionally alias-qualified as `alias.@attr`. The greedy ident
    // regex swallows the trailing `.`, so `s.@objectId` tokenizes as `Ident("s.")`
    // then `At("objectId")`; consume the optional trailing-dot prefix here so it
    // is not swallowed as a `Field`. A prefix of exactly one segment (`s.`) is the
    // FROM alias and is dropped. A MULTI-segment prefix (`s.value.`) carries
    // reference hops before the `@attr` tail (e.g. `s.value.@length`): the
    // segments are kept as raw `RefPath` hops here and `normalize_attr` strips the
    // leading alias hop once the FROM alias is known. Unknown names emit an
    // actionable error but still yield a placeholder so the `@`-token stays
    // committed (no backtracking into the bare-field arm, which would otherwise
    // mask the `unknown @attribute` message).
    let at_attr = select! { Token::Ident(p) if p.ends_with('.') => p }
        .or_not()
        .then(select! { Token::At(name) => name })
        .validate(|(prefix, name): (Option<String>, String), e, emitter| {
            let base = match name.as_str() {
                "objectId" => Attr::ObjectId,
                "objectAddress" => Attr::ObjectAddress,
                "usedHeapSize" => Attr::UsedHeapSize,
                "retainedHeapSize" | "retainedHeap" => Attr::RetainedHeapSize,
                "displayName" => Attr::DisplayName,
                "name" => Attr::DisplayName,
                "length" => Attr::Length,
                "inbounds" => Attr::Inbounds,
                "outbounds" => Attr::Outbounds,
                "valueArray" => Attr::ValueArray,
                "referenceArray" => Attr::ReferenceArray,
                other => {
                    emitter.emit(Rich::custom(
                        e.span(),
                        format!("unknown @attribute: @{other}"),
                    ));
                    Attr::ObjectId
                }
            };
            // A prefix with intermediate segments (more than the leading alias)
            // is a reference path: `s.value.@length` → hops carry `["s","value"]`
            // (raw); `normalize_attr` strips the leading alias. A single-segment
            // prefix (`s.`) is just the alias and carries no hops → bare attr.
            match prefix {
                Some(p) => {
                    let segs: Vec<String> =
                        p.trim_end_matches('.').split('.').map(str::to_string).collect();
                    if segs.len() >= 2 {
                        Attr::RefPath {
                            hops: segs,
                            tail: Box::new(base),
                            role: RefRole::ProjectionOnly,
                        }
                    } else {
                        base
                    }
                }
                None => base,
            }
        });
    let attr = at_attr
        .or(ident_ci("classof")
            .ignore_then(just(Token::LParen))
            .ignore_then(any_ident())
            .then_ignore(just(Token::RParen))
            .map(|_| Attr::ClassOf))
        .or(dom_fn("dominators").map(Attr::Dominators))
        .or(dom_fn("dominatorof").map(Attr::DominatorOf))
        .or(dom_fn("toString").map(Attr::ToString))
        .or(any_ident().map(Attr::Field))
        .labelled("attribute");

    // Arithmetic expression combinator: precedence-climbing over attr/literal primaries.
    // Defined here (after `attr`) so it can be reused in both the SELECT item and the
    // WHERE compare production. Bool/null ARE included as primaries so that compare
    // expressions like `flag = true` and `x != null` still parse. A bare single-leaf
    // expr is folded back to `SelectItem::Attr` by the caller; this combinator always
    // returns `Expr`.
    let expr = recursive(|expr| {
        let lit = select! {
            Token::Int(n) => Value::Int(n),
            Token::Float(f) => Value::Float(f),
            Token::Str(s) => Value::Str(s),
            Token::Ident(s) if s.eq_ignore_ascii_case("true") => Value::Bool(true),
            Token::Ident(s) if s.eq_ignore_ascii_case("false") => Value::Bool(false),
            Token::Ident(s) if s.eq_ignore_ascii_case("null") => Value::Null,
        }
        .map(Expr::Lit);

        let tohex = ident_ci("toHex")
            .ignore_then(just(Token::LParen))
            .ignore_then(expr.clone())
            .then_ignore(just(Token::RParen))
            .map(|arg| Expr::Attr(Attr::ToHex(Box::new(arg))));

        // `receiver.name(args)` — the greedy Ident regex swallows `s.getName` as a
        // single token. We match any dotted ident (contains `.`, does NOT end with
        // `.`) immediately followed by `(…)`, splitting on the last `.` to separate
        // the receiver alias from the method name. Zero-arg → empty Vec.
        let method_call = select! {
            Token::Ident(s) if s.contains('.') && !s.ends_with('.') => s
        }
        .then(
            just(Token::LParen)
                .ignore_then(
                    expr.clone()
                        .separated_by(just(Token::Comma))
                        .collect::<Vec<_>>(),
                )
                .then_ignore(just(Token::RParen)),
        )
        .map(|(dotted, args): (String, Vec<Expr>)| {
            let (recv, meth) = dotted.rsplit_once('.').unwrap();
            Expr::Method {
                receiver: Box::new(Expr::Attr(Attr::Field(recv.to_string()))),
                name: meth.to_string(),
                args,
            }
        });

        let primary = method_call
            .or(tohex)
            .or(lit)
            .or(just(Token::LParen)
                .ignore_then(expr.clone())
                .then_ignore(just(Token::RParen)))
            .or(attr.clone().map(Expr::Attr));

        let unary = just(Token::Minus)
            .to(UnaryOp::Neg)
            .or(just(Token::Plus).to(UnaryOp::Pos))
            .or_not()
            .then(primary)
            .map(|(u, p)| match u {
                None | Some(UnaryOp::Pos) => p,
                Some(UnaryOp::Neg) => match p {
                    Expr::Lit(Value::Int(n)) => Expr::Lit(Value::Int(-n)),
                    Expr::Lit(Value::Float(f)) => Expr::Lit(Value::Float(-f)),
                    other => Expr::Unary { op: UnaryOp::Neg, arg: Box::new(other) },
                },
            });

        let mul = unary.clone().foldl(
            just(Token::Star)
                .to(ArithOp::Mul)
                .or(just(Token::Divide).to(ArithOp::Div))
                .then(unary)
                .repeated(),
            |lhs, (op, rhs)| Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
        );

        mul.clone().foldl(
            just(Token::Plus)
                .to(ArithOp::Add)
                .or(just(Token::Minus).to(ArithOp::Sub))
                .then(mul)
                .repeated(),
            |lhs, (op, rhs)| Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
        )
    });

    // select item: AGG(item) | path(a, b) | toString(s) | * | expr (with single-leaf fold)
    // Each item may be followed by an optional `AS <name>` alias.
    // Guard: `AS RETAINED` is NOT consumed here — it belongs to the retained_set
    // modifier parsed at the SELECT level.
    let select_item = recursive(|item| {
        let agg = select! {
            Token::Ident(s) if agg_func(&s).is_some() => agg_func(&s).unwrap(),
        }
        .then_ignore(just(Token::LParen))
        .then(item.clone())
        .then_ignore(just(Token::RParen))
        .map(|(func, (arg, _alias)): (AggFunc, (SelectItem, Option<String>))| {
            (
                SelectItem::Aggregate {
                    func,
                    arg: Box::new(arg),
                },
                None::<String>,
            )
        });

        // `PERCENTILE(<arg>, <p>)` — two-arg aggregate. `p` is an integer literal
        // in 1..=100 (nearest-rank). Placed before `agg`; `PERCENTILE` is NOT in
        // `agg_func`, so the generic `agg` never matches it and a missing/extra arg
        // or an out-of-range `p` produces an actionable error instead of a generic
        // "unexpected token".
        let percentile_item = ident_ci("PERCENTILE")
            .ignore_then(just(Token::LParen))
            .ignore_then(item.clone())
            .then_ignore(just(Token::Comma))
            .then(select! { Token::Int(n) => n })
            .then_ignore(just(Token::RParen))
            .validate(|((arg, _alias), p): ((SelectItem, Option<String>), i64), e, emitter| {
                if !(1..=100).contains(&p) {
                    emitter.emit(Rich::custom(
                        e.span(),
                        format!(
                            "PERCENTILE(<arg>, p): p must be an integer between 1 and 100, got {p}"
                        ),
                    ));
                }
                let clamped = p.clamp(1, 100) as u8;
                (
                    SelectItem::Aggregate {
                        func: AggFunc::Percentile(clamped),
                        arg: Box::new(arg),
                    },
                    None::<String>,
                )
            });

        // `path(a, b)`. Contextual: only a path function when `path` is immediately
        // followed by `(`; otherwise `path` falls through to the bare-field attr arm.
        // Heuristic: an operand containing `.` or `*` (a dotted/globbed class name)
        // is a `Class`; any other bare ident is treated as an `Alias`.
        let path_operand = any_ident().map(|s: String| {
            if s.contains('.') || s.contains('*') {
                PathOperand::Class(s)
            } else {
                PathOperand::Alias(s)
            }
        });
        let path_item = ident_ci("path")
            .ignore_then(just(Token::LParen))
            .ignore_then(path_operand.clone())
            .then_ignore(just(Token::Comma))
            .then(path_operand)
            .then_ignore(just(Token::RParen))
            .map(|(from, to)| (SelectItem::Path { from, to }, None::<String>));

        let star = just(Token::Star).map(|_| (SelectItem::Star, None::<String>));

        // `toString(s)` as a SELECT item: `toString(alias)` → `SelectItem::ToString(alias)`.
        // Placed before the bare-attr fallback so `toString(` is consumed as ToString
        // rather than as a field named `toString`. The `dom_fn` helper enforces the
        // single-arg requirement with an actionable error.
        let tostring_item =
            dom_fn("toString").map(|a| (SelectItem::ToString(a), None::<String>));

        // `path_item` before the bare-attr fallback so `path(` is consumed as Path
        // rather than swallowed as a field named `path`.
        // `expr_item` covers all arithmetic expressions AND bare attrs (folded back).
        let expr_item = expr.clone().map(|e| {
            let item = match e {
                Expr::Attr(a) => SelectItem::Attr(a),
                other => SelectItem::Expr(Box::new(other)),
            };
            (item, None::<String>)
        });
        // IMPORTANT: `star` must come before `expr_item` so a lone `*` stays
        // `SelectItem::Star` (Star token is also ArithOp::Mul in expr; ordering wins).
        let base_item = percentile_item
            .or(agg)
            .or(path_item)
            .or(tostring_item)
            .or(star)
            .or(expr_item);

        // Optional `AS <alias>` suffix on any select item.
        // Safe-guard: do NOT match `AS RETAINED` (that belongs to the retained_set
        // modifier at the SELECT level). Use `.and_is(ident_ci("RETAINED").not())`
        // on the token immediately following `AS`.
        let alias_name = ident_ci("AS").ignore_then(
            select! { Token::Str(s) => s }
                .or(any_ident().and_is(ident_ci("RETAINED").not())),
        );

        base_item.then(alias_name.or_not()).map(|((item, _), alias)| (item, alias))
    });

    // Collect aliased items, then unzip into parallel vecs.
    let select_list = select_item
        .separated_by(just(Token::Comma))
        .at_least(1)
        .collect::<Vec<_>>()
        .map(|pairs: Vec<(SelectItem, Option<String>)>| {
            let (items, aliases): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
            (items, aliases)
        });

    // Symbolic comparison operators tokenized by logos.
    let sym_op = select! {
        Token::Eq => CompareOp::Eq,
        Token::Ne => CompareOp::Ne,
        Token::Lt => CompareOp::Lt,
        Token::Le => CompareOp::Le,
        Token::Gt => CompareOp::Gt,
        Token::Ge => CompareOp::Ge,
    };
    // Word operators `LIKE` and `NOT LIKE` (Java-regex full-match; see
    // `compare_values`). `NOT LIKE` is a two-token operator sequence that sits in
    // OPERATOR position (right after the attribute LHS), which is structurally
    // distinct from prefix `NOT` wrapping a whole predicate — prefix `NOT` is
    // handled by the `not` combinator before any attribute is seen, so the two
    // never collide. Try `NOT LIKE` before bare `LIKE`.
    let not_like = ident_ci("NOT")
        .ignore_then(ident_ci("LIKE"))
        .to(CompareOp::NotLike);
    let like = ident_ci("LIKE").to(CompareOp::Like);
    let op = sym_op.or(not_like).or(like).labelled("comparison operator");

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
            .ignore_then(base_query.clone())
            .then_ignore(just(Token::RParen))
            .map(|inner: Query| FromSource::Subquery(Box::new(inner)));
        let from_class = ident_ci("INSTANCEOF")
            .or_not()
            .map(|i| i.is_some())
            .then(
                any_ident()
                    .map(|name| (name, false))
                    .or(select! { Token::Str(s) => (s, true) }.labelled("class regex")),
            )
            .validate(|(instanceof, (class_name, is_regex)), e, emitter| {
                if instanceof && is_regex {
                    emitter.emit(Rich::custom(
                        e.span(),
                        "INSTANCEOF requires a bare class name, not a quoted regex \
                         (usage: FROM INSTANCEOF java.lang.Object, or drop INSTANCEOF \
                         and use FROM \"<regex>\" for regex matching)",
                    ));
                }
                FromSource::Class(ClassSpec {
                    instanceof,
                    class_name,
                    is_regex,
                })
            });
        // `FROM OBJECTS <address>`: a bare integer literal (decimal, hex, or
        // octal — all collapse to `Token::Int`) names one heap object by address.
        let from_object = select! { Token::Int(n) => n }.map(|n| FromSource::Object(n as u64));
        // `FROM INSTANCEOF <address>` is a common mistake: INSTANCEOF takes a
        // class, not an address. Detect INSTANCEOF followed by an int and emit an
        // actionable error rather than a bare parse failure.
        let instanceof_addr = ident_ci("INSTANCEOF")
            .ignore_then(select! { Token::Int(_) => () })
            .validate(|_, e, emitter| {
                emitter.emit(Rich::custom(
                    e.span(),
                    "INSTANCEOF <address> is not supported; use INSTANCEOF <class> \
                     (e.g. FROM INSTANCEOF java.lang.Thread)",
                ));
                FromSource::Object(0)
            });
        // `FROM OBJECTS (<expr>)` (arithmetic/boolean seed expressions) is a
        // deferred MAT feature. A leading `(` here (after OBJECTS was consumed and
        // it is NOT a `( SELECT ... )` subquery) means a not-yet-supported seed
        // expression. `from_subquery` is tried first (see `from_source`), so this
        // only fires for a `(` that failed to parse as a subquery.
        let from_object_expr = just(Token::LParen).rewind().validate(|_, e, emitter| {
            emitter.emit(Rich::custom(
                e.span(),
                "arithmetic/boolean FROM-OBJECTS expressions are not yet supported; \
                 use FROM OBJECTS <address> (e.g. FROM OBJECTS 0x1295e2f8) or FROM <class>",
            ));
            FromSource::Object(0)
        });
        let from_source = from_subquery
            .or(from_object)
            .or(instanceof_addr)
            .or(from_object_expr)
            .or(from_class);

        // Predicate grammar. Defined inside the `base_query` recursive closure so
        // the `IN (<subquery>)` alternative can reuse `base_query` for the inner
        // (non-correlated) query. UNION is unreachable inside the parens, so
        // `IN (... UNION ...)` fails to parse — the intended rejection.
        let predicate = recursive(|pred| {
            let paren = just(Token::LParen)
                .ignore_then(pred.clone())
                .then_ignore(just(Token::RParen));
            let instanceof = attr
                .clone()
                .then_ignore(ident_ci("INSTANCEOF"))
                .then(any_ident())
                .map(|(_lhs, cname)| Predicate::InstanceOf(cname));
            let in_subquery = attr
                .clone()
                .then_ignore(ident_ci("IN"))
                .then_ignore(just(Token::LParen))
                .then(base_query.clone())
                .then_ignore(just(Token::RParen))
                .map(|(lhs, inner): (Attr, Query)| Predicate::InSubquery {
                    lhs,
                    inner: Box::new(inner),
                });
            let compare = expr
                .clone()
                .then(op)
                .then(expr.clone())
                .validate(|((lhs, op), rhs), e, emitter| {
                    // LIKE/NOT LIKE RHS must be a string literal (MAT parity).
                    if matches!(op, CompareOp::Like | CompareOp::NotLike)
                        && !matches!(&rhs, Expr::Lit(Value::Str(_)))
                    {
                        emitter.emit(Rich::custom(
                            e.span(),
                            "LIKE right-hand side must be a string literal, \
                             e.g. LIKE \"java\\\\..*\"",
                        ));
                    }
                    Predicate::Compare { lhs, op, rhs }
                });
            // `in_subquery` before `compare` so `IN` isn't consumed as a bare field.
            let primary = paren.or(instanceof).or(in_subquery).or(compare);
            let not = recursive(|not| {
                ident_ci("NOT")
                    .ignore_then(not)
                    .map(|p| Predicate::Not(Box::new(p)))
                    .or(primary)
            });
            let and = not
                .clone()
                .foldl(ident_ci("AND").ignore_then(not).repeated(), |l, r| {
                    Predicate::And(Box::new(l), Box::new(r))
                });
            and.clone()
                .foldl(ident_ci("OR").ignore_then(and).repeated(), |l, r| {
                    Predicate::Or(Box::new(l), Box::new(r))
                })
        });

        ident_ci("SELECT")
            .ignore_then(ident_ci("DISTINCT").or_not().map(|d| d.is_some()))
            // Leading `AS RETAINED SET` (MAT also accepts this before the select list).
            .then(retained_set.clone())
            // `OBJECTS` between SELECT head and select list is a no-op projection marker.
            .then_ignore(ident_ci("OBJECTS").or_not())
            .then(select_list.clone())
            // Trailing `AS RETAINED SET` (existing MAT form, kept for compatibility).
            .then(retained_set.clone())
            .then_ignore(ident_ci("FROM"))
            // MAT allows `FROM OBJECTS <class>` as a no-op synonym for `FROM <class>`.
            .then_ignore(ident_ci("OBJECTS").or_not())
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
                    .ignore_then(
                        select! { Token::Int(n) if n >= 0 => n as u64 }.labelled("LIMIT count"),
                    )
                    .or_not(),
            )
            .map(
                |(
                    (((((((distinct, leading_retained), (select, select_aliases)), trailing_retained), from), alias), where_), order_by),
                    limit,
                )| {
                    let mut q = Query {
                        distinct,
                        select,
                        select_aliases,
                        retained_set: leading_retained || trailing_retained,
                        from,
                        alias,
                        where_,
                        order_by,
                        limit,
                        union_branches: Vec::new(),
                        union_limit: None,
                    };
                    // Now the alias is known, rewrite dotted `Field`s into N-hop
                    // `RefPath`s (a single segment after alias-strip stays a Field).
                    normalize_query_ref_paths(&mut q);
                    q
                },
            )
    });

    // Top level: a base query, then a flat `UNION`-separated tail folded into the
    // head's `union_branches`. Tail branches keep empty `union_branches` (the
    // list is flat, left-associative concatenation with UNION ALL semantics).
    //
    // A UNION branch may be bare (`UNION SELECT ...`) or parenthesized
    // (`UNION (SELECT ...)`, MAT's canonical form). Parens around a branch are
    // cosmetic; unwrap to the same `Query`. The `(` here sits at the top-level
    // UNION-branch position (before any SELECT), so it is unambiguous with the
    // FROM-subquery paren, which only follows `SELECT ... FROM`.
    let paren_branch = just(Token::LParen)
        .ignore_then(base_query.clone())
        .then_ignore(just(Token::RParen))
        .map(|q| (q, true)); // (branch, parenthesized?)
    let union_branch = paren_branch.or(base_query.clone().map(|q| (q, false)));
    // Optional trailing top-level LIMIT combinator, reused from the single-query
    // path (same `LIMIT <non-negative int>` shape). This only becomes reachable
    // AFTER a parenthesized last branch — a bare last branch's own `base_query`
    // greedily swallows any trailing LIMIT into that branch's `limit`, so the
    // top-level LIMIT here never fires for the bare form. We recover the bare-
    // form union-wide binding below by lifting the last branch's swallowed LIMIT.
    let trailing_limit = ident_ci("LIMIT")
        .ignore_then(select! { Token::Int(n) if n >= 0 => n as u64 }.labelled("LIMIT count"))
        .or_not();
    base_query
        .clone()
        .then(
            ident_ci("UNION")
                .ignore_then(union_branch)
                .repeated()
                .collect::<Vec<_>>(),
        )
        .then(trailing_limit)
        .then_ignore(end())
        .map(
            |((mut head, tail), trailing): ((Query, Vec<(Query, bool)>), Option<u64>)| {
                // DECISION (MAT gap #6): a trailing `LIMIT n` after a UNION binds
                // UNION-WIDE (applied to the whole concatenated result), matching
                // Eclipse MAT — NOT to a single branch. Two forms reach here:
                //   • parenthesized last branch `... UNION (SELECT ...) LIMIT n`:
                //     the `LIMIT n` sits at the top level and is captured in
                //     `trailing` directly.
                //   • bare last branch `... UNION SELECT ... LIMIT n`: the last
                //     branch's own `base_query` greedily absorbed the `LIMIT n`
                //     into the last branch's `limit`, so `trailing` is None. We
                //     LIFT that swallowed LIMIT up to the union level so the bare
                //     form matches MAT too. A LIMIT written INSIDE a branch's own
                //     parens is a genuine per-branch limit and is left untouched
                //     (we only lift when the last branch was NOT parenthesized).
                let last_was_paren = tail.last().map(|(_, p)| *p).unwrap_or(false);
                head.union_branches = tail.into_iter().map(|(q, _)| q).collect();
                if !head.union_branches.is_empty() {
                    if let Some(n) = trailing {
                        head.union_limit = Some(n);
                    } else if !last_was_paren {
                        // Bare-form: lift the last branch's swallowed LIMIT.
                        if let Some(last) = head.union_branches.last_mut() {
                            if let Some(n) = last.limit.take() {
                                head.union_limit = Some(n);
                            }
                        }
                    }
                }
                head
            },
        )
}

/// Rewrite dotted `Attr::Field` values in a query into N-hop `Attr::RefPath`s,
/// now that the FROM alias is known. A field whose text contains a `.` is a
/// reference path: strip a leading `<alias>.` (the alias denotes the FROM
/// object itself), then split the remainder on `.`. If ≥ 2 segments remain, the
/// last is the scalar/attr tail and the earlier ones are reference hops. A
/// single remaining segment stays a plain `Field`. Role defaults to
/// `ProjectionOnly`; the planner fixes it to `PredicateCritical` for WHERE uses.
/// Only touches the query's own clauses — subqueries are normalized when they
/// are themselves parsed.
fn normalize_query_ref_paths(q: &mut Query) {
    let alias = q.alias.clone();
    for item in &mut q.select {
        normalize_select_item(item, alias.as_deref());
    }
    if let Some(pred) = &mut q.where_ {
        normalize_predicate(pred, alias.as_deref());
    }
    if let Some(ob) = &mut q.order_by {
        normalize_attr(&mut ob.key, alias.as_deref());
    }
}

fn normalize_select_item(item: &mut SelectItem, alias: Option<&str>) {
    // If this item is exactly the bare alias name (no dot), rewrite it to Star —
    // the alias denotes the FROM object itself, just like `*`. This must fire
    // BEFORE the Attr branch's normalize_attr call so dotted paths (`s.field`)
    // are handled by normalize_attr while bare `s` becomes Star.
    if let SelectItem::Attr(Attr::Field(name)) = &*item {
        if let Some(al) = alias {
            if name == al && !name.contains('.') {
                *item = SelectItem::Star;
                return;
            }
        }
    }
    match item {
        SelectItem::Attr(a) => normalize_attr(a, alias),
        SelectItem::Aggregate { arg, .. } => normalize_select_item(arg, alias),
        SelectItem::Star => {}
        // `path(a, b)` operands are already resolved to Alias/Class at parse time;
        // they carry no dotted RefPath to normalize.
        SelectItem::Path { .. } => {}
        // `toString(s)` carries a single alias token; no dotted path to normalize.
        SelectItem::ToString(_) => {}
        SelectItem::Expr(e) => {
            normalize_expr(e, alias);
            // Lowering `e.getKey()`/`e.getValue()` in `normalize_expr` turns the
            // Method node into `Expr::Attr(RefPath)`. Fold that lone-attr Expr back
            // to `SelectItem::Attr` so it flows through the SAME planner/refwalk
            // path as `s.value.@length` (which the parser folds identically at
            // parse time via the `expr_item` combinator). Without this fold the
            // RefPath would sit inside a `SelectItem::Expr` that the refwalk hop
            // collector does not descend into.
            if let Expr::Attr(a) = e.as_ref() {
                *item = SelectItem::Attr(a.clone());
            }
        }
    }
}

fn normalize_predicate(pred: &mut Predicate, alias: Option<&str>) {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            normalize_predicate(a, alias);
            normalize_predicate(b, alias);
        }
        Predicate::Not(a) => normalize_predicate(a, alias),
        Predicate::Compare { lhs, rhs, .. } => {
            normalize_expr(lhs, alias);
            normalize_expr(rhs, alias);
        }
        // The inner query of an IN-subquery is normalized against its own alias
        // when it is parsed; the outer LHS attr is normalized here.
        Predicate::InSubquery { lhs, .. } => normalize_attr(lhs, alias),
        Predicate::InstanceOf(_) => {}
    }
}

/// Recurse into an `Expr`, normalizing all `Attr` leaves with `normalize_attr`.
fn normalize_expr(e: &mut Expr, alias: Option<&str>) {
    match e {
        Expr::Attr(a) => normalize_attr(a, alias),
        Expr::Lit(_) => {}
        Expr::Binary { lhs, rhs, .. } => {
            normalize_expr(lhs, alias);
            normalize_expr(rhs, alias);
        }
        Expr::Unary { arg, .. } => normalize_expr(arg, alias),
        Expr::Method { receiver, name, args } => {
            // Lower `e.getKey()` / `e.getValue()` (zero-arg) to a one-hop RefPath so
            // they reuse the RefWalk late-resolution pipeline exactly like
            // `s.value.@length`. MAT reflects into a live Map.Entry; our static
            // analog follows the backing `key`/`value` reference field one hop and
            // projects the resolved object's ADDRESS (identity) via `project_tail`.
            // Only the exact zero-arg getKey/getValue forms with a bare-alias field
            // receiver lower; every other method stays an `Expr::Method` and flows
            // through the existing scan-time `dispatch_method`/`emulate_jvm_method`.
            let hop_field = match name.as_str() {
                "getKey" => Some("key"),
                "getValue" => Some("value"),
                _ => None,
            };
            if let (Some(field), true, Expr::Attr(Attr::Field(recv))) =
                (hop_field, args.is_empty(), receiver.as_ref())
            {
                // Build hops carrying the receiver alias first (mirrors how a RefPath
                // built during parse carries the alias until `normalize_attr` strips
                // it). `recv` here is the bare receiver token (the FROM alias).
                let mut new_attr = Attr::RefPath {
                    hops: vec![recv.clone(), field.to_string()],
                    tail: Box::new(Attr::ObjectAddress),
                    role: RefRole::ProjectionOnly,
                };
                normalize_attr(&mut new_attr, alias);
                *e = Expr::Attr(new_attr);
                return;
            }
            normalize_expr(receiver, alias);
            for a in args { normalize_expr(a, alias); }
        }
    }
}

/// Rewrite a single `Attr::Field` into a `RefPath` when it is a multi-segment
/// reference path. Non-field attrs and single-segment fields are left as-is.
fn normalize_attr(a: &mut Attr, alias: Option<&str>) {
    // A `RefPath` built by `at_attr` (e.g. `s.value.@length`) carries its prefix
    // segments raw in `hops`, still including the leading FROM alias. Strip that
    // alias here now that it is known; a bare alias-only prefix (already filtered
    // in `at_attr`) never reaches this arm. If, after stripping, no hops remain,
    // the whole thing was just `alias.@attr` — collapse to the bare tail attr.
    if let Attr::RefPath { hops, tail, role } = a {
        if let Some(al) = alias {
            if hops.first().map(String::as_str) == Some(al) {
                hops.remove(0);
            }
        }
        if hops.is_empty() {
            *a = (**tail).clone();
        } else {
            // Recurse into the tail so a nested dotted-field tail is normalized too.
            let _ = role; // role is preserved as-is (planner may refine it).
            normalize_attr(tail, alias);
        }
        return;
    }
    let Attr::Field(name) = a else { return };
    if !name.contains('.') {
        return;
    }
    // Strip a leading `<alias>.` — the alias is the FROM object, not a hop.
    let stripped: &str = match alias {
        Some(al) => name
            .strip_prefix(al)
            .and_then(|rest| rest.strip_prefix('.'))
            .unwrap_or(name),
        None => name.as_str(),
    };
    let segs: Vec<&str> = stripped.split('.').collect();
    if segs.len() < 2 {
        // Single segment after alias-strip: a plain field. Replace the text with
        // the stripped form so `x.name` becomes `Field("name")`.
        *a = Attr::Field(stripped.to_string());
        return;
    }
    let (tail, hops) = segs.split_last().unwrap();
    *a = Attr::RefPath {
        hops: hops.iter().map(|s| s.to_string()).collect(),
        tail: Box::new(Attr::Field((*tail).to_string())),
        role: RefRole::ProjectionOnly,
    };
}

fn reserved_ident<'a, I>() -> impl Parser<'a, I, String, extra::Err<Rich<'a, Token>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(s) if is_reserved(&s) => s }
}

/// Clause/grammar keywords that open or structure a query but are not reserved
/// words in predicate position. `classof` lives in [`FUNCS`] (projection form).
pub const KEYWORDS: &[&str] = &["SELECT", "DISTINCT", "FROM"];

/// Words reserved in predicate/clause position (`is_reserved`'s source set).
pub const RESERVED: &[&str] = &[
    "WHERE",
    "LIMIT",
    "UNION",
    "AND",
    "OR",
    "NOT",
    "LIKE",
    "INSTANCEOF",
    "IN",
    "ORDER",
    "BY",
    "ASC",
    "DESC",
    // MAT no-op keyword; blocked from alias/completion slots.
    "OBJECTS",
    // Column-alias / retained-set modifier keywords.
    "AS",
    "RETAINED",
    "SET",
];

/// Aggregate function names (`agg_func`'s source set), upper-cased.
pub const AGG_FUNCS: &[&str] = &["COUNT", "SUM", "MIN", "MAX", "AVG", "PERCENTILE", "MEDIAN"];

/// Built-in scalar/graph function names used in SELECT/predicate position.
/// Source of truth for REPL completion; matches the `dom_fn` / `path` / `classof`
/// parser arms so completions can never drift from the grammar.
pub const FUNCS: &[&str] = &["classof", "toString", "toHex", "path", "dominators", "dominatorof"];

/// `@`-prefixed built-in attribute names (matching the `attr` parser's arms),
/// including the leading `@` so they can be offered as completions directly.
pub const ATTRIBUTES: &[&str] = &[
    "@objectId",
    "@objectAddress",
    "@usedHeapSize",
    "@retainedHeapSize",
    "@displayName",
    "@name",
    "@length",
    "@inbounds",
    "@outbounds",
    "@valueArray",
    "@referenceArray",
];

/// The union of every completion-candidate slice the parser exposes. The
/// context-aware REPL completer draws from the individual slices directly; this
/// helper exists so a test can assert the slices stay collectively exhaustive
/// and mutually disjoint (the single point of truth for keyword knowledge).
#[cfg(test)]
pub fn completion_words() -> Vec<&'static str> {
    KEYWORDS
        .iter()
        .chain(RESERVED.iter())
        .chain(AGG_FUNCS.iter())
        .chain(ATTRIBUTES.iter())
        .chain(FUNCS.iter())
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
        _ if s.eq_ignore_ascii_case("MEDIAN") => Some(AggFunc::Median),
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
        Predicate::Compare { lhs: Expr::Attr(lhs), op, rhs: Expr::Lit(rhs) }
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
        let n = select.len();
        Query {
            distinct,
            select,
            select_aliases: vec![None; n],
            retained_set: false,
            from: FromSource::Class(ClassSpec {
                instanceof,
                class_name: class_name.into(),
                is_regex: false,
            }),
            alias: alias.map(|s| s.into()),
            where_,
            order_by: None,
            limit,
            union_branches: Vec::new(),
            union_limit: None,
        }
    }
    fn star() -> Vec<SelectItem> {
        vec![SelectItem::Star]
    }
    fn attr_sel(a: Attr) -> SelectItem {
        SelectItem::Attr(a)
    }
    fn agg(func: AggFunc, arg: SelectItem) -> SelectItem {
        SelectItem::Aggregate {
            func,
            arg: Box::new(arg),
        }
    }

    #[test]
    fn parse_from_objects_numeric_id() {
        use super::FromSource;
        assert_eq!(super::parse("SELECT * FROM OBJECTS 1").unwrap().from, FromSource::Object(1));
        assert_eq!(super::parse("SELECT * FROM OBJECTS 0x10").unwrap().from, FromSource::Object(16));
        assert_eq!(super::parse("SELECT * FROM OBJECTS 0x0").unwrap().from, FromSource::Object(0));
    }
    #[test]
    fn reject_from_objects_expr_and_instanceof_addr() {
        let e = super::parse("SELECT * FROM OBJECTS (1 + 2)").unwrap_err();
        assert!(e.to_string().contains("arithmetic/boolean FROM-OBJECTS"), "got: {e}");
        let e = super::parse("SELECT * FROM INSTANCEOF 0x1").unwrap_err();
        assert!(e.to_string().to_lowercase().contains("instanceof"), "got: {e}");
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
                vec![
                    id("SELECT"),
                    Token::Star,
                    id("FROM"),
                    id("java.lang.String"),
                    id("s"),
                ],
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
                vec![
                    Token::Eq,
                    Token::Ne,
                    Token::Lt,
                    Token::Le,
                    Token::Gt,
                    Token::Ge,
                ],
            ),
            // 4: parens + comma
            ("( , )", vec![Token::LParen, Token::Comma, Token::RParen]),
            // 5: negative float lexes as Minus + Float (unary minus is grammar-level)
            ("-3.5", vec![Token::Minus, Token::Float(3.5)]),
            // 6: negative int lexes as Minus + Int (unary minus is grammar-level)
            ("-42", vec![Token::Minus, Token::Int(42)]),
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
            (
                "SELECT\t*\nFROM\rC",
                vec![id("SELECT"), Token::Star, id("FROM"), id("C")],
            ),
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
    // Group 1b — arithmetic operator lexing
    // ============================================================

    #[test]
    fn lexes_arithmetic_operators() {
        let toks: Vec<Token> = crate::query::parse::tokenize_spanned("@a + 2 - 3 * 4 / 5")
            .unwrap()
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(
            toks,
            vec![
                Token::At("a".into()),
                Token::Plus,
                Token::Int(2),
                Token::Minus,
                Token::Int(3),
                Token::Star,
                Token::Int(4),
                Token::Divide,
                Token::Int(5),
            ]
        );
    }

    #[test]
    fn minus_before_number_is_operator_not_negative_literal() {
        let toks: Vec<Token> = crate::query::parse::tokenize_spanned("1-2")
            .unwrap()
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(toks, vec![Token::Int(1), Token::Minus, Token::Int(2)]);
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
                q(
                    false,
                    star(),
                    false,
                    "java.lang.String",
                    Some("s"),
                    None,
                    None,
                ),
            ),
            // 2: star, no alias
            (
                "SELECT * FROM C",
                q(false, star(), false, "C", None, None, None),
            ),
            // 3: DISTINCT
            (
                "SELECT DISTINCT name FROM C",
                q(
                    false,
                    vec![attr_sel(field("name"))],
                    false,
                    "C",
                    None,
                    None,
                    None,
                )
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
                q(
                    false,
                    vec![attr_sel(Attr::ClassOf)],
                    false,
                    "java.lang.String",
                    Some("s"),
                    None,
                    None,
                ),
            ),
            // 10: COUNT(*)
            (
                "SELECT COUNT(*) FROM C",
                q(
                    false,
                    vec![agg(AggFunc::Count, SelectItem::Star)],
                    false,
                    "C",
                    None,
                    None,
                    None,
                ),
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
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(cmp(field("hash"), CompareOp::Gt, Value::Int(0))),
                    None,
                ),
            ),
            // 14: WHERE compare string
            (
                "SELECT * FROM C WHERE name = \"main\"",
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(cmp(field("name"), CompareOp::Eq, Value::Str("main".into()))),
                    None,
                ),
            ),
            // 15: WHERE compare float
            (
                "SELECT * FROM C WHERE ratio <= 1.5",
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(cmp(field("ratio"), CompareOp::Le, Value::Float(1.5))),
                    None,
                ),
            ),
            // 16: WHERE bool true
            (
                "SELECT * FROM C WHERE flag = true",
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(cmp(field("flag"), CompareOp::Eq, Value::Bool(true))),
                    None,
                ),
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
                q(
                    false,
                    star(),
                    false,
                    "C",
                    None,
                    Some(cmp(field("delta"), CompareOp::Eq, Value::Int(-7))),
                    None,
                ),
            ),
            // 24: nested aggregate arg (AGG over attr)
            (
                "SELECT COUNT(name) FROM C",
                q(
                    false,
                    vec![agg(AggFunc::Count, attr_sel(field("name")))],
                    false,
                    "C",
                    None,
                    None,
                    None,
                ),
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
            ("", "unexpected"),                             // 1 empty
            ("SELECT", "unexpected"),                       // 2 select only
            ("SELECT *", "unexpected"),                     // 3 missing FROM
            ("SELECT * FROM", "unexpected"),                // 4 FROM no class
            ("SELECT * FROM C bogus extra", "unexpected"),  // 5 trailing garbage
            ("SELECT @bogus FROM C", "bogus"),              // 6 unknown builtin attr
            ("SELECT * FROM C WHERE hash >", "unexpected"), // 7 dangling operator
            ("SELECT * FROM C WHERE hash", "unexpected"),   // 8 missing operator+rhs
            ("SELECT * FROM C LIMIT abc", "unexpected"),    // 9 non-int limit
            ("SELECT * FROM C LIMIT -1", "unexpected"),     // 10 negative limit rejected
            ("SELECT COUNT * FROM C", "unexpected"),        // 11 agg missing paren
            ("SELECT * FROM C WHERE (a = 1", "unexpected"), // 12 unbalanced paren
            ("SELECT , FROM C", "unexpected"),              // 13 empty select item
            ("SELECT * FROM C WHERE a = ", "unexpected"),   // 14 missing rhs value
            ("SELECT * FROM C WHERE a == 1", "unexpected"), // 15 bad operator ==
        ];
        for (src, needle) in cases {
            let err = parse(src)
                .err()
                .unwrap_or_else(|| panic!("expected parse error for {src:?}"))
                .0;
            assert!(!err.is_empty(), "empty error for {src:?}");
            assert!(
                !err.contains('\n'),
                "expected single-line error for {src:?}, got: {err}"
            );
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
        let q =
            parse("SELECT * FROM java.lang.String UNION SELECT * FROM java.lang.Integer").unwrap();
        assert_eq!(q.union_branches.len(), 1);
        assert_eq!(q.union_branches[0].from.class_name(), "java.lang.Integer");
        assert!(
            q.union_branches[0].union_branches.is_empty(),
            "branches must be flat, not nested"
        );
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

    // ---------- top-level union-wide LIMIT (MAT gap #6) ----------

    #[test]
    fn union_wide_limit_parenthesized_form() {
        // MAT applies a trailing LIMIT to the WHOLE union. With a parenthesized
        // last branch, the trailing `LIMIT 5` sits at the top level and must land
        // in `union_limit`, NOT on any branch.
        let q =
            parse("SELECT * FROM java.lang.String UNION (SELECT * FROM java.lang.Object) LIMIT 5")
                .unwrap();
        assert_eq!(q.union_branches.len(), 1, "head + 1 branch");
        assert_eq!(
            q.union_limit,
            Some(5),
            "trailing LIMIT after `)` must be union-wide"
        );
        // The per-branch limit must be untouched (the LIMIT is not inside the parens).
        assert_eq!(q.limit, None, "head branch keeps no per-branch LIMIT");
        assert_eq!(q.union_branches[0].limit, None, "branch keeps no LIMIT");
    }

    #[test]
    fn union_wide_limit_bare_form_binds_union_wide() {
        // DECISION (pinned): the bare form `... UNION SELECT ... LIMIT 5` binds the
        // trailing LIMIT UNION-WIDE (to match Eclipse MAT), NOT to the last branch.
        // A bare branch has no closing token, so the LIMIT is parsed at the top
        // level after the branch tail and stored in `union_limit`.
        let q = parse("SELECT * FROM A UNION SELECT * FROM B LIMIT 5").unwrap();
        assert_eq!(q.union_branches.len(), 1);
        assert_eq!(
            q.union_limit,
            Some(5),
            "bare-form trailing LIMIT is union-wide"
        );
        assert_eq!(q.limit, None, "head keeps no per-branch LIMIT");
        assert_eq!(
            q.union_branches[0].limit, None,
            "last branch must NOT absorb the union-wide LIMIT"
        );
    }

    #[test]
    fn union_wide_limit_absent_is_none() {
        // No trailing LIMIT → union_limit stays None (old behavior, OVERALL cap only).
        let q = parse("SELECT * FROM A UNION SELECT * FROM B").unwrap();
        assert_eq!(q.union_limit, None);
    }

    #[test]
    fn single_query_union_limit_is_none() {
        // A non-union query never sets union_limit, even with a per-branch LIMIT.
        let q = parse("SELECT * FROM C LIMIT 5").unwrap();
        assert_eq!(q.union_limit, None, "single query has no union_limit");
        assert_eq!(q.limit, Some(5), "single-query LIMIT stays per-query");
    }

    #[test]
    fn union_wide_limit_zero_parses() {
        let q = parse("SELECT * FROM A UNION SELECT * FROM B LIMIT 0").unwrap();
        assert_eq!(q.union_limit, Some(0));
    }

    // ---------- PERCENTILE / MEDIAN aggregates ----------

    #[test]
    fn median_parses_as_single_arg_aggregate() {
        let q = parse("SELECT MEDIAN(@usedHeapSize) FROM C").unwrap();
        assert_eq!(
            q.select,
            vec![agg(AggFunc::Median, attr_sel(Attr::UsedHeapSize))]
        );
    }

    #[test]
    fn percentile_parses_with_integer_arg() {
        let q = parse("SELECT PERCENTILE(@usedHeapSize, 95) FROM C").unwrap();
        assert_eq!(
            q.select,
            vec![agg(AggFunc::Percentile(95), attr_sel(Attr::UsedHeapSize))]
        );
    }

    #[test]
    fn percentile_boundary_values_parse() {
        assert_eq!(
            parse("SELECT PERCENTILE(@usedHeapSize, 1) FROM C")
                .unwrap()
                .select,
            vec![agg(AggFunc::Percentile(1), attr_sel(Attr::UsedHeapSize))]
        );
        assert_eq!(
            parse("SELECT PERCENTILE(@usedHeapSize, 100) FROM C")
                .unwrap()
                .select,
            vec![agg(AggFunc::Percentile(100), attr_sel(Attr::UsedHeapSize))]
        );
    }

    #[test]
    fn percentile_out_of_range_is_actionable_error() {
        for bad in ["0", "101", "200"] {
            let src = format!("SELECT PERCENTILE(@usedHeapSize, {bad}) FROM C");
            let err = parse(&src).unwrap_err().0;
            assert!(
                err.contains("between 1 and 100"),
                "p={bad} should give an actionable range error, got: {err}"
            );
        }
    }

    #[test]
    fn percentile_missing_second_arg_is_error() {
        // A lone-arg PERCENTILE is not a valid MEDIAN alias; the comma+int are
        // required and its absence must not silently fall through to a field.
        assert!(parse("SELECT PERCENTILE(@usedHeapSize) FROM C").is_err());
    }

    #[test]
    fn percentile_case_insensitive() {
        let q = parse("SELECT percentile(@usedHeapSize, 50) FROM C").unwrap();
        assert_eq!(
            q.select,
            vec![agg(AggFunc::Percentile(50), attr_sel(Attr::UsedHeapSize))]
        );
    }

    #[test]
    fn union_branch_inner_limit_preserved_with_union_wide_limit() {
        // A parenthesized branch may carry its OWN LIMIT (inside the parens) AND
        // the whole union may carry a trailing union-wide LIMIT after the `)`.
        let q = parse("SELECT * FROM A UNION (SELECT * FROM B LIMIT 3) LIMIT 5").unwrap();
        assert_eq!(q.union_branches.len(), 1);
        assert_eq!(
            q.union_branches[0].limit,
            Some(3),
            "the branch's own LIMIT (inside parens) is preserved"
        );
        assert_eq!(q.union_limit, Some(5), "trailing LIMIT is union-wide");
    }

    // ---------- parenthesized UNION branches (MAT canonical form) ----------

    #[test]
    fn union_parenthesized_branch_parses() {
        let q = parse("SELECT * FROM java.lang.String UNION (SELECT * FROM java.lang.Integer)")
            .unwrap();
        assert_eq!(q.union_branches.len(), 1);
        let branch = &q.union_branches[0];
        assert_eq!(branch.from.class_name(), "java.lang.Integer");
        assert_eq!(branch.select, vec![SelectItem::Star]);
        assert!(
            branch.union_branches.is_empty(),
            "branches must be flat, not nested"
        );
    }

    #[test]
    fn union_parenthesized_branch_equals_bare_branch() {
        // A parenthesized branch unwraps to the SAME branch AST as the bare form.
        let bare =
            parse("SELECT * FROM java.lang.String UNION SELECT * FROM java.lang.Integer").unwrap();
        let paren = parse("SELECT * FROM java.lang.String UNION (SELECT * FROM java.lang.Integer)")
            .unwrap();
        assert_eq!(bare.union_branches, paren.union_branches);
    }

    #[test]
    fn union_bare_branch_still_parses() {
        // Regression: the bare (unparenthesized) branch form still works.
        let q = parse("SELECT * FROM A UNION SELECT * FROM B").unwrap();
        assert_eq!(q.union_branches.len(), 1);
        assert_eq!(q.union_branches[0].from.class_name(), "B");
    }

    #[test]
    fn union_multiple_parenthesized_branches() {
        let q = parse("SELECT * FROM A UNION (SELECT * FROM B) UNION (SELECT * FROM C)").unwrap();
        assert_eq!(q.union_branches.len(), 2);
        assert_eq!(q.union_branches[0].from.class_name(), "B");
        assert_eq!(q.union_branches[1].from.class_name(), "C");
        assert!(q.union_branches.iter().all(|b| b.union_branches.is_empty()));
    }

    #[test]
    fn union_mixed_bare_and_parenthesized_branches() {
        let q = parse("SELECT * FROM A UNION (SELECT * FROM B) UNION SELECT * FROM C").unwrap();
        assert_eq!(q.union_branches.len(), 2);
        assert_eq!(q.union_branches[0].from.class_name(), "B");
        assert_eq!(q.union_branches[1].from.class_name(), "C");
    }

    #[test]
    fn union_parenthesized_branch_with_where_and_limit() {
        // The full base_query grammar is available inside the branch parens.
        let q =
            parse("SELECT * FROM A UNION (SELECT * FROM B b WHERE b.hash > 0 LIMIT 5)").unwrap();
        assert_eq!(q.union_branches.len(), 1);
        let branch = &q.union_branches[0];
        assert_eq!(branch.from.class_name(), "B");
        assert!(branch.where_.is_some(), "branch WHERE must be populated");
        assert_eq!(branch.limit, Some(5), "branch LIMIT must be populated");
    }

    #[test]
    fn union_unterminated_parenthesized_branch_errors() {
        // Missing `)` after a parenthesized branch is an actionable parse error.
        let err = parse("SELECT * FROM A UNION (SELECT * FROM B")
            .unwrap_err()
            .0;
        assert!(
            !err.is_empty(),
            "expected non-empty error for unterminated UNION branch"
        );
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
        assert!(
            q.from.instanceof(),
            "INSTANCEOF flag must survive migration"
        );
        assert_eq!(q.from.class_name(), "java.util.List");
    }

    // ---------- MAT gap #5: quoted/regex FROM class pattern ----------

    #[test]
    fn from_quoted_string_is_regex() {
        let q = parse(r#"SELECT * FROM "java.lang.*""#).unwrap();
        let spec = q.from.class_spec().expect("class source");
        assert_eq!(
            spec,
            &ClassSpec {
                instanceof: false,
                class_name: "java.lang.*".into(),
                is_regex: true,
            }
        );
    }

    #[test]
    fn from_quoted_alternation_regex() {
        let q = parse(r#"SELECT * FROM ".*Ab.*|java.lang.Runtime""#).unwrap();
        let spec = q.from.class_spec().expect("class source");
        assert!(spec.is_regex, "double-quoted FROM must set is_regex");
        assert_eq!(spec.class_name, ".*Ab.*|java.lang.Runtime");
    }

    #[test]
    fn from_bare_ident_is_not_regex() {
        let q = parse("SELECT * FROM java.lang.String").unwrap();
        let spec = q.from.class_spec().expect("class source");
        assert!(!spec.is_regex, "bare-ident FROM must NOT be regex");
    }

    #[test]
    fn from_bare_glob_is_not_regex() {
        let q = parse("SELECT * FROM com.acme.*").unwrap();
        let spec = q.from.class_spec().expect("class source");
        assert!(!spec.is_regex, "bare-glob FROM must NOT be regex");
        assert_eq!(spec.class_name, "com.acme.*");
    }

    #[test]
    fn instanceof_with_quoted_regex_is_rejected() {
        let err = parse(r#"SELECT * FROM INSTANCEOF "java.lang.*""#)
            .expect_err("INSTANCEOF with a quoted regex must be rejected");
        assert!(
            err.0.contains("INSTANCEOF") && err.0.to_lowercase().contains("bare class name"),
            "error must actionably explain INSTANCEOF needs a bare class name; got: {}",
            err.0
        );
    }

    #[test]
    fn instanceof_with_bare_ident_still_valid() {
        let q = parse("SELECT * FROM INSTANCEOF java.util.List").unwrap();
        let spec = q.from.class_spec().expect("class source");
        assert!(spec.instanceof);
        assert!(!spec.is_regex);
    }

    #[test]
    fn from_quoted_regex_with_alias() {
        let q = parse(r#"SELECT * FROM "java\.lang\..*" s"#).unwrap();
        let spec = q.from.class_spec().expect("class source");
        assert!(spec.is_regex);
        assert_eq!(spec.class_name, r"java\.lang\..*");
        assert_eq!(q.alias.as_deref(), Some("s"));
    }

    #[test]
    fn refpath_two_hops_parses() {
        let q = parse("SELECT x.parent.name FROM Node x").unwrap();
        match &q.select[0] {
            SelectItem::Attr(Attr::RefPath { hops, tail, .. }) => {
                assert_eq!(hops, &vec!["parent".to_string()]);
                assert!(matches!(**tail, Attr::Field(ref f) if f == "name"));
            }
            other => panic!("expected RefPath, got {other:?}"),
        }
    }

    #[test]
    fn refpath_bare_single_segment_stays_field() {
        // `x.name` — one segment after alias-strip — is a plain field, not a RefPath.
        let q = parse("SELECT x.name FROM Node x").unwrap();
        match &q.select[0] {
            SelectItem::Attr(Attr::Field(f)) => assert_eq!(f, "name"),
            other => panic!("expected bare Field, got {other:?}"),
        }
    }

    #[test]
    fn refpath_without_alias_keeps_leading_segment() {
        // No alias bound: `a.b.c` has three segments, all real hops+tail.
        let q = parse("SELECT a.b.c FROM Node").unwrap();
        match &q.select[0] {
            SelectItem::Attr(Attr::RefPath { hops, tail, .. }) => {
                assert_eq!(hops, &vec!["a".to_string(), "b".to_string()]);
                assert!(matches!(**tail, Attr::Field(ref f) if f == "c"));
            }
            other => panic!("expected RefPath, got {other:?}"),
        }
    }

    #[test]
    fn refpath_in_where_parses() {
        let q = parse("SELECT * FROM Node x WHERE x.parent.id = 7").unwrap();
        match q.where_.as_ref().unwrap() {
            Predicate::Compare {
                lhs: Expr::Attr(Attr::RefPath { hops, tail, .. }),
                ..
            } => {
                assert_eq!(hops, &vec!["parent".to_string()]);
                assert!(matches!(**tail, Attr::Field(ref f) if f == "id"));
            }
            other => panic!("expected RefPath compare, got {other:?}"),
        }
    }

    #[test]
    fn union_inside_subquery_is_rejected() {
        // UNION is unreachable inside the parenthesized base_query, so the inner
        // UNION fails to parse (located error) rather than silently nesting.
        let err = parse("SELECT * FROM (SELECT * FROM A UNION SELECT * FROM B) x")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unexpected"),
            "expected a located parse error, got: {err}"
        );
        assert!(
            err.contains(':'),
            "error should carry a line:col location, got: {err}"
        );
    }

    #[test]
    fn in_subquery_parses() {
        let q = parse("SELECT * FROM java.lang.String s WHERE @objectAddress IN (SELECT * FROM java.lang.Integer)").unwrap();
        match q.where_.as_ref().unwrap() {
            Predicate::InSubquery { lhs, inner } => {
                assert!(matches!(lhs, Attr::ObjectAddress));
                assert_eq!(inner.from.class_name(), "java.lang.Integer");
                assert!(inner.union_branches.is_empty());
            }
            other => panic!("expected InSubquery, got {other:?}"),
        }
    }

    #[test]
    fn in_subquery_combines_with_and() {
        // `IN (...)` composes as a normal primary inside a boolean predicate.
        let q = parse(
            "SELECT * FROM java.lang.String s WHERE hash > 0 AND @objectAddress IN (SELECT * FROM C)",
        )
        .unwrap();
        match q.where_.as_ref().unwrap() {
            Predicate::And(l, r) => {
                assert!(matches!(**l, Predicate::Compare { .. }));
                assert!(matches!(**r, Predicate::InSubquery { .. }));
            }
            other => panic!("expected AND(compare, InSubquery), got {other:?}"),
        }
    }

    #[test]
    fn in_subquery_on_object_id_parses() {
        let q =
            parse("SELECT * FROM C WHERE @objectId IN (SELECT @objectId FROM java.lang.Integer)")
                .unwrap();
        match q.where_.as_ref().unwrap() {
            Predicate::InSubquery { lhs, inner } => {
                assert!(matches!(lhs, Attr::ObjectId));
                assert_eq!(inner.from.class_name(), "java.lang.Integer");
            }
            other => panic!("expected InSubquery, got {other:?}"),
        }
    }

    #[test]
    fn union_inside_in_subquery_is_rejected() {
        // UNION is unreachable inside the parenthesized base_query, so a UNION
        // within an IN-subquery is a located parse error, not a silent nesting.
        let err = parse(
            "SELECT * FROM C WHERE @objectAddress IN (SELECT * FROM A UNION SELECT * FROM B)",
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("unexpected"),
            "expected a located parse error, got: {err}"
        );
        assert!(
            err.contains(':'),
            "error should carry a line:col location, got: {err}"
        );
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
        assert!(
            err.to_string().contains("dominators(x) requires"),
            "unexpected error: {err}"
        );
    }
    #[test]
    fn parse_dominatorof_requires_arg() {
        let err = parse("SELECT dominatorof() FROM java.lang.String s").unwrap_err();
        assert!(
            err.to_string().contains("dominatorof(x) requires"),
            "unexpected error: {err}"
        );
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
        assert!(
            rep.contains("dominatorof(x) requires"),
            "report missing message: {rep}"
        );
    }

    // ---------- @inbounds / @outbounds + path(a, b) ----------

    #[test]
    fn parse_inbounds_attr() {
        let q = parse("SELECT @inbounds FROM C").unwrap();
        assert_eq!(q.select, vec![attr_sel(Attr::Inbounds)]);
    }
    #[test]
    fn parse_outbounds_attr() {
        let q = parse("SELECT @outbounds FROM C").unwrap();
        assert_eq!(q.select, vec![attr_sel(Attr::Outbounds)]);
    }
    #[test]
    fn inbounds_usable_in_where() {
        // Goes through the same `attr` parser, so it is a valid compare LHS.
        let q = parse("SELECT @objectId FROM C WHERE @inbounds > 0").unwrap();
        match q.where_.as_ref().unwrap() {
            Predicate::Compare { lhs, .. } => assert_eq!(lhs.as_attr().expect("Expr::Attr"), &Attr::Inbounds),
            other => panic!("expected compare on @inbounds, got {other:?}"),
        }
    }
    #[test]
    fn outbounds_usable_in_where() {
        let q = parse("SELECT @objectId FROM C WHERE @outbounds != 0").unwrap();
        match q.where_.as_ref().unwrap() {
            Predicate::Compare { lhs, .. } => assert_eq!(lhs.as_attr().expect("Expr::Attr"), &Attr::Outbounds),
            other => panic!("expected compare on @outbounds, got {other:?}"),
        }
    }

    #[test]
    fn parse_path_alias_and_class() {
        // `path(s, java.lang.Thread)`: bare `s` → Alias, dotted → Class.
        let q = parse("SELECT path(s, java.lang.Thread) FROM C s").unwrap();
        assert_eq!(
            q.select,
            vec![SelectItem::Path {
                from: PathOperand::Alias("s".into()),
                to: PathOperand::Class("java.lang.Thread".into()),
            }]
        );
    }
    #[test]
    fn parse_path_both_aliases() {
        // Two bare idents (no `.`, no `*`) → both Alias.
        let q = parse("SELECT path(a, b) FROM C").unwrap();
        assert_eq!(
            q.select,
            vec![SelectItem::Path {
                from: PathOperand::Alias("a".into()),
                to: PathOperand::Alias("b".into()),
            }]
        );
    }
    #[test]
    fn parse_path_both_classes() {
        let q = parse("SELECT path(java.lang.String, java.lang.Integer) FROM C").unwrap();
        assert_eq!(
            q.select,
            vec![SelectItem::Path {
                from: PathOperand::Class("java.lang.String".into()),
                to: PathOperand::Class("java.lang.Integer".into()),
            }]
        );
    }
    #[test]
    fn parse_path_globbed_operand_is_class() {
        // A glob (`*`) marks a class pattern even without a `.`.
        let q = parse("SELECT path(s, com.acme.*) FROM C s").unwrap();
        assert_eq!(
            q.select,
            vec![SelectItem::Path {
                from: PathOperand::Alias("s".into()),
                to: PathOperand::Class("com.acme.*".into()),
            }]
        );
    }
    #[test]
    fn path_bare_field_without_parens_stays_field() {
        // `path` NOT followed by `(` is an ordinary field name (contextual ident).
        let q = parse("SELECT path FROM C").unwrap();
        assert_eq!(q.select, vec![attr_sel(field("path"))]);
    }
    #[test]
    fn path_dotted_field_without_parens_stays_field() {
        // `x.path` after alias-strip is a single-segment Field named `path`.
        let q = parse("SELECT x.path FROM C x").unwrap();
        assert_eq!(q.select, vec![attr_sel(field("path"))]);
    }
    #[test]
    fn path_coexists_with_other_select_items() {
        let q = parse("SELECT @objectId, path(s, C) FROM C s").unwrap();
        assert_eq!(
            q.select,
            vec![
                attr_sel(Attr::ObjectId),
                SelectItem::Path {
                    from: PathOperand::Alias("s".into()),
                    to: PathOperand::Alias("C".into()),
                },
            ]
        );
    }
    #[test]
    fn path_one_operand_is_error() {
        // `path(s)` — a single operand — must be a parse error, not silently accepted.
        let err = parse("SELECT path(s) FROM C s").unwrap_err().0;
        assert!(!err.is_empty(), "expected non-empty error for path(s)");
        assert!(
            !err.contains('\n'),
            "expected single-line error, got: {err}"
        );
    }
    #[test]
    fn inbounds_outbounds_in_attributes_const() {
        assert!(
            ATTRIBUTES.contains(&"@inbounds"),
            "ATTRIBUTES must include @inbounds"
        );
        assert!(
            ATTRIBUTES.contains(&"@outbounds"),
            "ATTRIBUTES must include @outbounds"
        );
    }

    #[test]
    fn parse_as_retained_set() {
        let q = parse("SELECT s AS RETAINED SET FROM java.lang.String s").unwrap();
        assert!(q.retained_set);
        assert_eq!(q.select.len(), 1);
    }
    #[test]
    fn parse_no_retained_set_default_false() {
        assert!(
            !parse("SELECT s FROM java.lang.String s")
                .unwrap()
                .retained_set
        );
    }
    #[test]
    fn parse_as_retained_missing_set() {
        let err = parse("SELECT s AS RETAINED FROM java.lang.String s").unwrap_err();
        assert!(
            err.to_string().contains("expected SET after 'AS RETAINED'"),
            "unexpected: {err}"
        );
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
        assert!(
            parse("SELECT s as retained set FROM C s")
                .unwrap()
                .retained_set
        );
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
        assert!(
            rep.contains("query:1:"),
            "expected caret location, got:\n{rep}"
        );
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
            // PERCENTILE is a two-arg aggregate with its own parser production
            // (not routed through `agg_func`), so it is validated separately.
            if f.eq_ignore_ascii_case("PERCENTILE") {
                assert!(
                    agg_func(f).is_none(),
                    "PERCENTILE should not be a single-arg agg_func"
                );
                assert!(
                    parse(&format!("SELECT {f}(@usedHeapSize, 50) FROM C")).is_ok(),
                    "parser rejects two-arg aggregate {f:?}"
                );
                continue;
            }
            assert!(
                agg_func(f).is_some(),
                "agg_func rejects declared AGG_FUNC {f:?}"
            );
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
            assert!(
                is_reserved(r),
                "is_reserved rejects declared RESERVED {r:?}"
            );
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
        for set in [KEYWORDS, RESERVED, AGG_FUNCS, ATTRIBUTES, FUNCS] {
            for &w in set {
                assert!(words.contains(&w), "completion_words missing {w:?}");
            }
        }
        let unique: std::collections::HashSet<_> = words.iter().collect();
        assert_eq!(words.len(), unique.len(), "completion_words has duplicates");
    }

    // ============================================================
    // Group — alias-qualified @attr (MAT: `s.@objectId`)
    // ============================================================

    #[test]
    fn parse_prefixed_at_attr_single_select() {
        let q = parse("SELECT s.@objectId FROM java.lang.String s").expect("should parse");
        assert_eq!(q.select, vec![attr_sel(Attr::ObjectId)]);
    }

    #[test]
    fn parse_multi_dot_prefixed_at_attr_builds_refpath() {
        // `a.b.@objectId` lexes as `Ident("a.b.")` + `At`; the leading alias `a`
        // is stripped and the remaining `b` becomes a reference hop, with the
        // `@objectId` as the RefPath tail (previously the whole prefix was
        // dropped, silently discarding the `b` hop).
        let q = parse("SELECT a.b.@objectId FROM java.lang.Object a").expect("should parse");
        assert_eq!(
            q.select,
            vec![attr_sel(Attr::RefPath {
                hops: vec!["b".to_string()],
                tail: Box::new(Attr::ObjectId),
                role: RefRole::ProjectionOnly,
            })],
        );
    }

    #[test]
    fn parse_prefixed_at_attr_with_dotted_field() {
        let q = parse("SELECT s.@objectId, s.hash FROM java.lang.String s").expect("should parse");
        assert_eq!(
            q.select,
            vec![attr_sel(Attr::ObjectId), attr_sel(field("hash"))],
            "prefix dropped from @attr; `s.hash` alias-stripped to Field(\"hash\")"
        );
    }

    #[test]
    fn parse_value_length_builds_refpath_with_length_tail() {
        // `s.value.@length` must resolve the `value` ref hop and project the
        // walked-to array's @length (an identity attr tail on a RefPath). The
        // leading alias `s` is stripped; `value` remains as the single hop.
        let q = parse("SELECT s.value.@length FROM java.lang.String s").expect("should parse");
        assert_eq!(
            q.select,
            vec![attr_sel(Attr::RefPath {
                hops: vec!["value".to_string()],
                tail: Box::new(Attr::Length),
                role: RefRole::ProjectionOnly,
            })],
        );
    }

    #[test]
    fn parse_at_attr_tail_in_where_builds_refpath() {
        // A `@length` tail on a RefPath is also valid inside WHERE.
        let q = parse("SELECT s FROM java.lang.String s WHERE s.value.@length > 3")
            .expect("should parse");
        match q.where_.as_ref().expect("where") {
            Predicate::Compare {
                lhs: Expr::Attr(Attr::RefPath { hops, tail, .. }),
                ..
            } => {
                assert_eq!(hops, &vec!["value".to_string()]);
                assert!(matches!(**tail, Attr::Length));
            }
            other => panic!("expected RefPath compare, got {other:?}"),
        }
    }

    #[test]
    fn parse_bare_at_attr_still_works() {
        let q = parse("SELECT @objectId FROM java.lang.String").expect("should parse");
        assert_eq!(q.select, vec![attr_sel(Attr::ObjectId)]);
    }

    #[test]
    fn parse_prefixed_at_attr_mixed_columns() {
        let q = parse(
            "SELECT s.@objectAddress, s.@usedHeapSize, s.@retainedHeapSize FROM java.lang.Object s",
        )
        .expect("should parse");
        assert_eq!(
            q.select,
            vec![
                attr_sel(Attr::ObjectAddress),
                attr_sel(Attr::UsedHeapSize),
                attr_sel(Attr::RetainedHeapSize),
            ]
        );
    }

    #[test]
    fn parse_prefixed_at_attr_unknown_name_errors() {
        let err = parse("SELECT s.@bogus FROM X s").expect_err("unknown @attr should error");
        assert!(
            err.0.contains("unknown @attribute"),
            "actionable error expected, got: {}",
            err.0
        );
    }

    #[test]
    fn parse_prefixed_at_attr_in_where() {
        let q =
            parse("SELECT * FROM java.lang.String s WHERE s.@objectId = 0").expect("should parse");
        assert_eq!(
            q.where_,
            Some(cmp(Attr::ObjectId, CompareOp::Eq, Value::Int(0)))
        );
    }

    #[test]
    fn parse_prefixed_at_attr_alias_name_not_hardcoded() {
        let q = parse("SELECT obj.@objectId FROM java.lang.Object obj").expect("should parse");
        assert_eq!(q.select, vec![attr_sel(Attr::ObjectId)]);
    }

    #[test]
    fn parse_prefixed_at_attr_in_order_by() {
        let q = parse("SELECT * FROM java.lang.String s ORDER BY s.@retainedHeapSize DESC")
            .expect("should parse");
        assert_eq!(
            q.order_by.map(|o| (o.key, o.dir)),
            Some((Attr::RetainedHeapSize, SortDir::Desc))
        );
    }

    #[test]
    fn parse_like_operator() {
        let q = parse(r#"SELECT * FROM C WHERE name LIKE "m.*""#).unwrap();
        assert_eq!(
            q.where_.as_ref().unwrap(),
            &cmp(field("name"), CompareOp::Like, Value::Str("m.*".into()))
        );
    }

    #[test]
    fn parse_like_case_insensitive_keyword() {
        let q = parse(r#"SELECT * FROM C WHERE name like "m.*""#).unwrap();
        assert_eq!(
            q.where_.as_ref().unwrap(),
            &cmp(field("name"), CompareOp::Like, Value::Str("m.*".into()))
        );
    }

    #[test]
    fn parse_not_like_operator() {
        let q = parse(r#"SELECT * FROM C WHERE name NOT LIKE "m.*""#).unwrap();
        assert_eq!(
            q.where_.as_ref().unwrap(),
            &cmp(field("name"), CompareOp::NotLike, Value::Str("m.*".into()))
        );
    }

    #[test]
    fn parse_not_like_case_insensitive_keyword() {
        let q = parse(r#"SELECT * FROM C WHERE name not like "m.*""#).unwrap();
        assert_eq!(
            q.where_.as_ref().unwrap(),
            &cmp(field("name"), CompareOp::NotLike, Value::Str("m.*".into()))
        );
    }

    // CRITICAL DISAMBIGUATION: prefix `NOT` that wraps a whole predicate must
    // still parse as `Not(Compare{Eq})`, NOT be confused with the `NOT LIKE`
    // operator (which sits in operator position after an attribute LHS).
    #[test]
    fn parse_prefix_not_still_wraps_predicate() {
        let q = parse(r#"SELECT * FROM C WHERE NOT name = "x""#).unwrap();
        assert_eq!(
            q.where_.as_ref().unwrap(),
            &not(cmp(field("name"), CompareOp::Eq, Value::Str("x".into())))
        );
    }

    // Prefix NOT wrapping a LIKE compare — `NOT (name LIKE "x")`, distinct from
    // `name NOT LIKE "x"`. Both are valid but structurally different.
    #[test]
    fn parse_prefix_not_wrapping_like() {
        let q = parse(r#"SELECT * FROM C WHERE NOT name LIKE "m.*""#).unwrap();
        assert_eq!(
            q.where_.as_ref().unwrap(),
            &not(cmp(
                field("name"),
                CompareOp::Like,
                Value::Str("m.*".into())
            ))
        );
    }

    #[test]
    fn parse_like_combines_with_and() {
        let q = parse(r#"SELECT * FROM C WHERE name LIKE "m.*" AND id = 1"#).unwrap();
        assert_eq!(
            q.where_.as_ref().unwrap(),
            &and(
                cmp(field("name"), CompareOp::Like, Value::Str("m.*".into())),
                cmp(field("id"), CompareOp::Eq, Value::Int(1))
            )
        );
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

    // ============================================================
    // toString(s) parser AST tests (MAT gap #3)
    // ============================================================

    /// `SELECT toString(s) FROM java.lang.String s` must parse so the single
    /// SELECT item is exactly `SelectItem::ToString("s")`.
    #[test]
    fn parse_tostring_select_item() {
        let q = parse("SELECT toString(s) FROM java.lang.String s").unwrap();
        assert_eq!(q.select.len(), 1);
        match &q.select[0] {
            SelectItem::ToString(alias) => assert_eq!(alias, "s"),
            other => panic!("expected SelectItem::ToString(\"s\"), got {other:?}"),
        }
        assert_eq!(q.alias.as_deref(), Some("s"));
    }

    /// `SELECT * FROM java.lang.String s WHERE toString(s) LIKE "java.*"` must
    /// parse so the WHERE predicate is exactly `Compare { lhs: Attr::ToString("s"),
    /// op: CompareOp::Like, rhs: Value::Str("java.*") }`.
    #[test]
    fn parse_tostring_in_where_like() {
        let q =
            parse(r#"SELECT * FROM java.lang.String s WHERE toString(s) LIKE "java.*""#).unwrap();
        match q.where_.as_ref().unwrap() {
            Predicate::Compare { lhs, op, rhs } => {
                assert_eq!(
                    lhs.as_attr().expect("Compare lhs is Expr::Attr"),
                    &Attr::ToString("s".into()),
                    "WHERE LHS must be Attr::ToString(\"s\")"
                );
                assert_eq!(*op, CompareOp::Like, "operator must be Like");
                assert_eq!(
                    rhs.as_lit().expect("Compare rhs is Expr::Lit"),
                    &Value::Str("java.*".into()),
                    "RHS must be Str(\"java.*\")"
                );
            }
            other => panic!("expected Compare predicate, got {other:?}"),
        }
    }

    /// Pin the behavior of bare `toString` (without parens/arg) in a SELECT.
    /// The parser treats `toString` NOT followed by `(` as a plain field name
    /// `Attr::Field("toString")`, since the `dom_fn` combinator is contextual:
    /// it only fires when the identifier is immediately followed by `(`.
    #[test]
    fn parse_bare_tostring_without_parens_is_field_not_function() {
        let q = parse("SELECT toString FROM java.lang.String").unwrap();
        assert_eq!(q.select.len(), 1);
        match &q.select[0] {
            SelectItem::Attr(Attr::Field(name)) => assert_eq!(name, "toString"),
            other => panic!(
                "bare `toString` without parens must be Attr::Field(\"toString\"), got {other:?}"
            ),
        }
    }

    /// `toString()` — parens but NO alias argument — must be a parse error with
    /// an actionable message naming the function and the required argument form.
    #[test]
    fn parse_tostring_requires_alias_arg() {
        let err = parse("SELECT toString() FROM java.lang.String s").unwrap_err();
        assert!(
            err.to_string().contains("toString(x) requires"),
            "expected actionable error, got: {err}"
        );
    }

    /// `toString` is case-insensitive: `TOSTRING(s)` and `ToString(s)` both
    /// parse identically to `toString(s)`.
    #[test]
    fn parse_tostring_case_insensitive() {
        for variant in &["TOSTRING(s)", "ToString(s)", "tostring(s)"] {
            let src = format!("SELECT {variant} FROM java.lang.String s");
            let q = parse(&src).unwrap_or_else(|e| panic!("parse failed for {src:?}: {e}"));
            match &q.select[0] {
                SelectItem::ToString(alias) => assert_eq!(alias, "s", "for {variant}"),
                other => panic!("expected ToString for {variant}, got {other:?}"),
            }
        }
    }

    // ============================================================
    // Group — FROM OBJECTS keyword (MAT gap: 266 occurrences in OQLTest.java)
    // ============================================================

    /// `SELECT * FROM OBJECTS java.lang.String` must parse to the SAME AST as
    /// `SELECT * FROM java.lang.String` — OBJECTS is a no-op marker.
    #[test]
    fn from_objects_bare_class_parses() {
        let with_objects = parse("SELECT * FROM OBJECTS java.lang.String").unwrap();
        let without = parse("SELECT * FROM java.lang.String").unwrap();
        assert_eq!(
            with_objects.from, without.from,
            "FROM OBJECTS <class> must produce identical FROM as FROM <class>"
        );
    }

    /// The class_name and is_regex fields must be correct after OBJECTS.
    #[test]
    fn from_objects_class_name_and_is_regex() {
        let q = parse("SELECT * FROM OBJECTS java.lang.String").unwrap();
        let spec = q.from.class_spec().expect("expected class source");
        assert_eq!(
            spec.class_name, "java.lang.String",
            "class_name must be java.lang.String after OBJECTS"
        );
        assert!(
            !spec.is_regex,
            "bare-ident after OBJECTS must not be is_regex"
        );
        assert!(!spec.instanceof, "OBJECTS alone must not set instanceof");
    }

    /// `SELECT COUNT(*) FROM OBJECTS java.lang.String` — the most common MAT form.
    #[test]
    fn from_objects_count_star_parses() {
        let q = parse("SELECT COUNT(*) FROM OBJECTS java.lang.String").unwrap();
        assert_eq!(
            q.from.class_name(),
            "java.lang.String",
            "FROM OBJECTS class_name must be java.lang.String"
        );
    }

    /// `FROM OBJECTS` is case-insensitive: `from objects` and `FROM OBJECTS` both work.
    #[test]
    fn from_objects_case_insensitive() {
        for variant in &[
            "SELECT * FROM OBJECTS java.lang.String",
            "SELECT * FROM objects java.lang.String",
            "SELECT * FROM Objects java.lang.String",
        ] {
            let q = parse(variant).unwrap_or_else(|e| panic!("parse failed for {variant:?}: {e}"));
            assert_eq!(
                q.from.class_name(),
                "java.lang.String",
                "FROM {variant} must yield class java.lang.String"
            );
        }
    }

    /// `FROM OBJECTS ( <subquery> )` — OBJECTS before a parenthesized subquery.
    #[test]
    fn from_objects_subquery_parses() {
        let q = parse("SELECT * FROM OBJECTS ( SELECT * FROM java.lang.String )").unwrap();
        match &q.from {
            FromSource::Subquery(inner) => {
                assert_eq!(
                    inner.from.class_name(),
                    "java.lang.String",
                    "inner subquery must have class java.lang.String"
                );
            }
            other => panic!("expected FromSource::Subquery after OBJECTS, got {other:?}"),
        }
    }

    /// `FROM OBJECTS "java\.lang\..*"` — quoted/regex class after OBJECTS must still
    /// set is_regex = true.
    #[test]
    fn from_objects_quoted_regex_sets_is_regex() {
        let q = parse(r#"SELECT * FROM OBJECTS "java\.lang\..*""#).unwrap();
        let spec = q.from.class_spec().expect("expected class source");
        assert!(
            spec.is_regex,
            "quoted class after OBJECTS must set is_regex = true"
        );
        assert_eq!(spec.class_name, r"java\.lang\..*");
    }

    /// `FROM OBJECTS java.lang.String` with an alias — alias is correctly captured.
    #[test]
    fn from_objects_with_alias() {
        let q = parse("SELECT s FROM OBJECTS java.lang.String s").unwrap();
        assert_eq!(q.from.class_name(), "java.lang.String");
        assert_eq!(q.alias.as_deref(), Some("s"));
    }

    /// FROM OBJECTS INSTANCEOF: accepted as no-op (OBJECTS consumed first, then
    /// INSTANCEOF sets its flag). Eclipse MAT rejects this combination, but chumsky
    /// has no clean combinator path to reject it. This test pins the accepted behavior.
    #[test]
    fn from_objects_instanceof_is_accepted_as_noop() {
        let result = parse("SELECT * FROM OBJECTS INSTANCEOF java.lang.String");
        match result {
            Ok(q) => {
                assert_eq!(
                    q.from.class_name(),
                    "java.lang.String",
                    "FROM OBJECTS INSTANCEOF: class must be java.lang.String"
                );
                assert!(
                    q.from.instanceof(),
                    "FROM OBJECTS INSTANCEOF: instanceof flag must be set"
                );
            }
            Err(e) => {
                panic!(
                    "FROM OBJECTS INSTANCEOF currently accepted as no-op, \
                     but got parse error: {e}\n\
                     If you intentionally added rejection, update this test to \
                     assert the actionable error message."
                );
            }
        }
    }

    /// A bare `objects` used as a WHERE field must still work — RESERVED only blocks
    /// the alias slot, not predicate fields (which use `any_ident()`).
    #[test]
    fn objects_as_where_field_still_parses() {
        // `objects` used as a plain field name in WHERE is not affected by RESERVED.
        let q = parse("SELECT * FROM C WHERE objects = 1").unwrap();
        match q.where_.as_ref().unwrap() {
            Predicate::Compare { lhs, .. } => {
                assert_eq!(
                    lhs.as_attr().expect("Compare lhs is Expr::Attr"),
                    &Attr::Field("objects".into()),
                    "objects as a WHERE field must parse as Attr::Field(\"objects\")"
                );
            }
            other => panic!("expected Compare predicate, got {other:?}"),
        }
    }

    /// OBJECTS must appear in the RESERVED array — it should never be offered as
    /// a class-name or alias completion.
    #[test]
    fn objects_is_in_reserved() {
        assert!(
            RESERVED.iter().any(|&r| r.eq_ignore_ascii_case("OBJECTS")),
            "OBJECTS must be in RESERVED (guards alias-position and completion drift)"
        );
        assert!(
            is_reserved("OBJECTS"),
            "is_reserved(\"OBJECTS\") must return true"
        );
        assert!(
            is_reserved("objects"),
            "is_reserved is case-insensitive; must return true for \"objects\""
        );
    }

    /// `FROM OBJECTS` with a glob class pattern — should work identically to bare FROM.
    #[test]
    fn from_objects_glob_class_parses() {
        let with_objects = parse("SELECT * FROM OBJECTS java.util.*").unwrap();
        let without = parse("SELECT * FROM java.util.*").unwrap();
        assert_eq!(
            with_objects.from, without.from,
            "FROM OBJECTS glob must produce identical FROM as FROM glob"
        );
    }

    // ============================================================
    // Group N — AS <name> column alias tests
    // ============================================================

    /// 1. Bare-ident alias on a dotted-attr select item.
    #[test]
    fn alias_bare_ident_on_attr() {
        let q = parse("SELECT s.@objectId AS foo FROM java.lang.String s").unwrap();
        assert_eq!(q.select_aliases.len(), 1);
        assert_eq!(q.select_aliases[0].as_deref(), Some("foo"));
        assert_eq!(q.select, vec![SelectItem::Attr(Attr::ObjectId)]);
    }

    /// 2. Quoted alias name.
    #[test]
    fn alias_quoted_string() {
        let q = parse(r#"SELECT @usedHeapSize AS "size" FROM java.lang.String"#).unwrap();
        assert_eq!(q.select_aliases[0].as_deref(), Some("size"));
        assert_eq!(q.select, vec![SelectItem::Attr(Attr::UsedHeapSize)]);
    }

    /// 3. REGRESSION: `SELECT s AS RETAINED SET FROM ...` must NOT treat
    ///    RETAINED as an alias — retained_set must be true and select == [s].
    #[test]
    fn alias_as_retained_set_regression() {
        let q = parse("SELECT s AS RETAINED SET FROM java.lang.String s").unwrap();
        assert!(
            q.retained_set,
            "retained_set must be true when AS RETAINED SET is used"
        );
        // After the bare-alias fix, `s` (the FROM alias) rewrites to Star.
        assert_eq!(
            q.select,
            vec![SelectItem::Star],
            "select must be [Star] — bare alias s rewrites to Star"
        );
        assert_eq!(
            q.select_aliases[0], None,
            "item must carry no alias (RETAINED was not consumed as alias name)"
        );
    }

    /// 3b. Case-insensitive RETAINED guard: lower-case `as retained set` also
    ///     must not be treated as an alias.
    #[test]
    fn alias_as_retained_set_case_insensitive_regression() {
        let q = parse("SELECT s as retained set FROM java.lang.String s").unwrap();
        assert!(q.retained_set);
        assert_eq!(q.select_aliases[0], None);
    }

    /// 4. Alias on aggregate.
    #[test]
    fn alias_on_aggregate() {
        let q = parse("SELECT COUNT(*) AS n FROM java.lang.String").unwrap();
        assert_eq!(q.select_aliases[0].as_deref(), Some("n"));
        assert!(
            matches!(&q.select[0], SelectItem::Aggregate { func: AggFunc::Count, .. }),
            "select must be COUNT(*)"
        );
    }

    /// 5. No alias → select_aliases entry is None; derived name unchanged.
    #[test]
    fn no_alias_means_none() {
        let q = parse("SELECT @objectId FROM java.lang.String").unwrap();
        assert_eq!(q.select_aliases.len(), 1);
        assert_eq!(q.select_aliases[0], None);
    }

    /// 6. Cross-phase attr (@retainedHeapSize) can still be aliased.
    #[test]
    fn alias_on_retained_heap_size() {
        let q = parse("SELECT @retainedHeapSize AS r FROM java.lang.String").unwrap();
        assert_eq!(q.select_aliases[0].as_deref(), Some("r"));
        assert_eq!(q.select, vec![SelectItem::Attr(Attr::RetainedHeapSize)]);
    }

    /// Multiple aliased columns in one SELECT.
    #[test]
    fn multiple_aliased_columns() {
        let q =
            parse("SELECT @objectId AS id, @usedHeapSize AS bytes FROM java.lang.String").unwrap();
        assert_eq!(q.select_aliases.len(), 2);
        assert_eq!(q.select_aliases[0].as_deref(), Some("id"));
        assert_eq!(q.select_aliases[1].as_deref(), Some("bytes"));
    }

    /// Mixed: first column has alias, second does not.
    #[test]
    fn mixed_aliased_and_plain_columns() {
        let q = parse("SELECT @objectId AS id, @usedHeapSize FROM java.lang.String").unwrap();
        assert_eq!(q.select_aliases[0].as_deref(), Some("id"));
        assert_eq!(q.select_aliases[1], None);
    }

    /// Alias combined with ORDER BY.
    #[test]
    fn alias_combined_with_order_by() {
        let q =
            parse("SELECT @usedHeapSize AS bytes FROM java.lang.String ORDER BY @usedHeapSize DESC")
                .unwrap();
        assert_eq!(q.select_aliases[0].as_deref(), Some("bytes"));
        assert!(q.order_by.is_some());
    }

    /// Alias after a `path(a,b)` item.
    #[test]
    fn alias_on_path_item() {
        let q = parse(
            "SELECT path(s, java.lang.Object) AS p FROM java.lang.String s",
        )
        .unwrap();
        assert_eq!(q.select_aliases[0].as_deref(), Some("p"));
        assert!(matches!(&q.select[0], SelectItem::Path { .. }));
    }

    /// AS with a quoted name that looks like a reserved word.
    #[test]
    fn alias_quoted_reserved_word_name() {
        let q = parse(r#"SELECT * AS "FROM" FROM java.lang.String"#).unwrap();
        assert_eq!(q.select_aliases[0].as_deref(), Some("FROM"));
    }

    /// UNION: alias on head branch is present; tail branch alias is independent.
    #[test]
    fn alias_union_head_branch_preserved() {
        let q =
            parse("SELECT @objectId AS id FROM java.lang.String UNION SELECT @objectId FROM java.lang.Object")
                .unwrap();
        assert_eq!(q.select_aliases[0].as_deref(), Some("id"));
        // tail branch has no alias
        assert_eq!(q.union_branches[0].select_aliases[0], None);
    }

    /// select_aliases vec is always the same length as select vec.
    #[test]
    fn select_aliases_length_matches_select() {
        let queries = [
            "SELECT * FROM C",
            "SELECT @objectId, @usedHeapSize FROM C",
            "SELECT COUNT(*) AS n, SUM(@usedHeapSize) AS total FROM C",
        ];
        for oql in &queries {
            let q = parse(oql).unwrap();
            assert_eq!(
                q.select.len(),
                q.select_aliases.len(),
                "select_aliases length mismatch for: {oql}"
            );
        }
    }

    // ============================================================
    // Group: SELECT OBJECTS + leading AS RETAINED SET (Task 3)
    // ============================================================

    /// 1. SELECT OBJECTS <s> is a no-op projection marker: parses, select == [Star]
    ///    because `s` is the FROM alias and rewrites to Star.
    #[test]
    fn select_objects_is_noop() {
        let q = parse("SELECT OBJECTS s FROM java.lang.String s").unwrap();
        assert_eq!(
            q.select,
            vec![SelectItem::Star],
            "OBJECTS must be a no-op: select must be [Star] (bare alias rewrites to Star)"
        );
        assert!(!q.retained_set, "OBJECTS must not set retained_set");
        assert!(!q.distinct, "OBJECTS must not set distinct");
    }

    /// 2. Leading AS RETAINED SET: SELECT AS RETAINED SET s FROM ...
    #[test]
    fn leading_as_retained_set() {
        let q = parse("SELECT AS RETAINED SET s FROM java.lang.String s").unwrap();
        assert!(
            q.retained_set,
            "leading AS RETAINED SET must set retained_set"
        );
        // bare alias `s` rewrites to Star after the SW-1 fix
        assert_eq!(
            q.select,
            vec![SelectItem::Star],
            "select must be [Star] with leading retained (bare alias rewrites)"
        );
    }

    /// 3. SELECT OBJECTS produces the SAME select vec as SELECT (no OBJECTS).
    #[test]
    fn select_objects_same_as_select() {
        let with_objects = parse("SELECT OBJECTS s FROM java.lang.String s").unwrap();
        let without_objects = parse("SELECT s FROM java.lang.String s").unwrap();
        assert_eq!(
            with_objects.select, without_objects.select,
            "OBJECTS must be a pure no-op: selects must be identical"
        );
        assert_eq!(
            with_objects.retained_set, without_objects.retained_set,
            "OBJECTS must not change retained_set"
        );
        assert_eq!(
            with_objects.distinct, without_objects.distinct,
            "OBJECTS must not change distinct"
        );
    }

    /// 4. REGRESSION: trailing AS RETAINED SET still parses.
    #[test]
    fn trailing_as_retained_set_regression() {
        let q = parse("SELECT s AS RETAINED SET FROM java.lang.String s").unwrap();
        assert!(
            q.retained_set,
            "trailing AS RETAINED SET must still set retained_set"
        );
        // bare alias `s` rewrites to Star after the SW-1 fix
        assert_eq!(
            q.select,
            vec![SelectItem::Star],
            "select must be [Star] for trailing form (bare alias rewrites)"
        );
    }

    /// 5. Leading AS RETAINED without SET shares the actionable error message.
    #[test]
    fn leading_as_retained_missing_set_errors() {
        let err = parse("SELECT AS RETAINED s FROM java.lang.String s").unwrap_err();
        assert!(
            err.to_string().contains("expected SET after 'AS RETAINED'"),
            "leading missing-SET error must be actionable, got: {err}"
        );
    }

    /// 6. SELECT DISTINCT OBJECTS: both DISTINCT and OBJECTS together.
    #[test]
    fn select_distinct_objects() {
        let q = parse("SELECT DISTINCT OBJECTS s FROM java.lang.String s").unwrap();
        assert!(q.distinct, "distinct must be true");
        // bare alias `s` rewrites to Star after the SW-1 fix
        assert_eq!(
            q.select,
            vec![SelectItem::Star],
            "select must be [Star] with DISTINCT OBJECTS (bare alias rewrites)"
        );
        assert!(!q.retained_set);
    }

    /// 7. Leading AS RETAINED SET + OBJECTS before select list.
    #[test]
    fn leading_as_retained_set_objects() {
        let q =
            parse("SELECT AS RETAINED SET OBJECTS s FROM java.lang.String s").unwrap();
        assert!(
            q.retained_set,
            "retained_set must be true with leading AS RETAINED SET OBJECTS"
        );
        // bare alias `s` rewrites to Star after the SW-1 fix
        assert_eq!(
            q.select,
            vec![SelectItem::Star],
        );
    }

    /// 8. Case-insensitive: `select objects s from java.lang.String s` parses.
    #[test]
    fn select_objects_case_insensitive() {
        let q = parse("select objects s from java.lang.String s").unwrap();
        // bare alias `s` rewrites to Star after the SW-1 fix
        assert_eq!(
            q.select,
            vec![SelectItem::Star],
        );
    }

    /// 9. Leading + trailing both present: SELECT AS RETAINED SET s AS RETAINED SET FROM ...
    ///    Both set retained_set=true; combined result is true.
    #[test]
    fn leading_and_trailing_as_retained_set_both_true() {
        let q = parse("SELECT AS RETAINED SET s AS RETAINED SET FROM java.lang.String s")
            .expect("leading + trailing AS RETAINED SET must parse Ok");
        assert!(
            q.retained_set,
            "when both leading and trailing present, retained_set must be true"
        );
    }

    /// 10. OBJECTS before an aggregate: SELECT OBJECTS COUNT(*) FROM C
    ///     Pin the observed accept/reject behaviour.
    #[test]
    fn select_objects_before_aggregate() {
        let q = parse("SELECT OBJECTS COUNT(*) FROM java.lang.String")
            .expect("OBJECTS before aggregate must parse Ok");
        assert!(
            matches!(
                &q.select[0],
                SelectItem::Aggregate { func: AggFunc::Count, .. }
            ),
            "COUNT(*) after OBJECTS must still parse as aggregate"
        );
    }

    /// 11. SELECT OBJECTS * (star after OBJECTS).
    #[test]
    fn select_objects_star() {
        let q = parse("SELECT OBJECTS * FROM java.lang.String").unwrap();
        assert_eq!(q.select, vec![SelectItem::Star]);
    }

    /// 12. SELECT DISTINCT AS RETAINED SET: distinct + leading retained together.
    #[test]
    fn select_distinct_as_retained_set_leading() {
        let q = parse("SELECT DISTINCT AS RETAINED SET s FROM java.lang.String s").unwrap();
        assert!(q.distinct, "distinct must be true");
        assert!(q.retained_set, "retained_set must be true");
        // bare alias `s` rewrites to Star after the SW-1 fix
        assert_eq!(
            q.select,
            vec![SelectItem::Star],
        );
    }

    /// 13. SELECT AS RETAINED SET DISTINCT (wrong order) — pin reject behaviour.
    #[test]
    fn select_as_retained_set_distinct_wrong_order_pinned() {
        // DISTINCT must come before AS RETAINED SET in this grammar.
        // `SELECT AS RETAINED SET DISTINCT s` is not accepted.
        let r = parse("SELECT AS RETAINED SET DISTINCT s FROM java.lang.String s");
        // Pin: this does NOT parse successfully (DISTINCT is consumed as the select
        // expression item, or the grammar rejects it).
        // Either an error, or `distinct` is NOT set on the query.
        if let Ok(q) = r {
            assert!(
                !q.distinct,
                "DISTINCT after AS RETAINED SET should NOT set distinct flag (wrong order)"
            );
        }
        // If it errors, that is also correct behaviour — wrong order is not supported.
    }

    // ============================================================
    // Group: arithmetic expression grammar (Task 3)
    // ============================================================

    fn parse_one(s: &str) -> Query {
        super::parse(s).unwrap_or_else(|e| panic!("parse failed for {s:?}: {}", e.0))
    }

    #[test]
    fn arithmetic_precedence_mul_binds_tighter_than_add() {
        let q = parse_one("SELECT @usedHeapSize + @length * 2 FROM C");
        match &q.select[0] {
            SelectItem::Expr(e) => match e.as_ref() {
                Expr::Binary { op: ArithOp::Add, lhs, rhs } => {
                    assert!(matches!(lhs.as_ref(), Expr::Attr(_)));
                    assert!(matches!(rhs.as_ref(), Expr::Binary { op: ArithOp::Mul, .. }));
                }
                other => panic!("expected Add root, got {other:?}"),
            },
            other => panic!("expected Expr item, got {other:?}"),
        }
    }

    #[test]
    fn arithmetic_parens_override_precedence() {
        let q = parse_one("SELECT (@usedHeapSize + @length) * 2 FROM C");
        match &q.select[0] {
            SelectItem::Expr(e) => assert!(matches!(e.as_ref(), Expr::Binary { op: ArithOp::Mul, .. })),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn unary_minus_on_attr_parses() {
        let q = parse_one("SELECT -@usedHeapSize FROM C");
        match &q.select[0] {
            SelectItem::Expr(e) => assert!(matches!(e.as_ref(), Expr::Unary { op: UnaryOp::Neg, .. })),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn lone_star_stays_select_star_not_multiply() {
        let q = parse_one("SELECT * FROM C");
        assert_eq!(q.select, vec![SelectItem::Star]);
    }

    #[test]
    fn bare_attr_folds_to_attr_item_not_expr() {
        let q = parse_one("SELECT @usedHeapSize FROM C");
        assert_eq!(q.select, vec![SelectItem::Attr(Attr::UsedHeapSize)]);
    }

    #[test]
    fn where_arithmetic_both_sides() {
        let q = parse_one("SELECT * FROM C WHERE @usedHeapSize / 8 > @length + 1");
        match q.where_.unwrap() {
            Predicate::Compare { lhs, op: CompareOp::Gt, rhs } => {
                assert!(matches!(lhs, Expr::Binary { op: ArithOp::Div, .. }));
                assert!(matches!(rhs, Expr::Binary { op: ArithOp::Add, .. }));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn where_plain_compare_folds_to_leaf_exprs() {
        let q = parse_one("SELECT * FROM C WHERE @usedHeapSize > 100");
        match q.where_.unwrap() {
            Predicate::Compare { lhs, op: CompareOp::Gt, rhs } => {
                assert_eq!(lhs, Expr::Attr(Attr::UsedHeapSize));
                assert_eq!(rhs, Expr::Lit(Value::Int(100)));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn negative_literal_folds_via_unary() {
        let q = parse_one("SELECT * FROM C WHERE delta = -5");
        match q.where_.unwrap() {
            Predicate::Compare { rhs, .. } => assert_eq!(rhs, Expr::Lit(Value::Int(-5))),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn like_rhs_must_be_string_literal() {
        let err = super::parse("SELECT * FROM C WHERE name LIKE @a + 1").unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.to_lowercase().contains("like"), "error should mention LIKE: {msg}");
    }

    // ============================================================
    // Group: bare-alias rewrite in SELECT (SW-1 / SW-3 fix)
    // ============================================================

    /// `SELECT s FROM java.lang.String s` — the bare alias `s` must rewrite to
    /// `SelectItem::Star` (project the object itself), not `Attr::Field("s")`.
    #[test]
    fn bare_alias_in_select_rewrites_to_star() {
        let q = parse_one("SELECT s FROM java.lang.String s");
        assert_eq!(
            q.select,
            vec![SelectItem::Star],
            "bare alias s must rewrite to Star, got {:?}",
            q.select
        );
    }

    /// `SELECT COUNT(s) FROM java.lang.String s` — the aggregate arg `s` must
    /// also rewrite to `Star` so COUNT(s) becomes COUNT(*) semantics.
    #[test]
    fn bare_alias_in_count_arg_rewrites_to_star() {
        let q = parse_one("SELECT COUNT(s) FROM java.lang.String s");
        match &q.select[0] {
            SelectItem::Aggregate { func: AggFunc::Count, arg } => {
                assert_eq!(
                    arg.as_ref(),
                    &SelectItem::Star,
                    "COUNT(s) arg must rewrite to Star, got {:?}",
                    arg
                );
            }
            other => panic!("expected COUNT aggregate, got {other:?}"),
        }
    }

    /// REGRESSION: `SELECT s.count FROM java.lang.String s` — dotted path must
    /// NOT be affected; it should normalize to `Attr::Field("count")`.
    #[test]
    fn dotted_alias_field_is_not_rewritten_to_star() {
        let q = parse_one("SELECT s.count FROM java.lang.String s");
        assert_eq!(
            q.select,
            vec![SelectItem::Attr(Attr::Field("count".into()))],
            "s.count must normalize to Field(\"count\"), got {:?}",
            q.select
        );
    }

    /// REGRESSION: `SELECT count FROM java.lang.String s` — a bare name that is
    /// NOT the alias must stay `Attr::Field("count")`.
    #[test]
    fn bare_non_alias_field_stays_field() {
        let q = parse_one("SELECT count FROM java.lang.String s");
        assert_eq!(
            q.select,
            vec![SelectItem::Attr(Attr::Field("count".into()))],
            "bare non-alias field must stay Field(\"count\"), got {:?}",
            q.select
        );
    }

    /// The alias-qualified `@attr` form (`s.@objectId`) still produces
    /// `Attr::ObjectId` — not affected by the bare-alias rewrite.
    #[test]
    fn alias_qualified_at_attr_still_works() {
        let q = parse_one("SELECT s.@objectId FROM java.lang.String s");
        assert_eq!(
            q.select,
            vec![SelectItem::Attr(Attr::ObjectId)],
            "s.@objectId must remain Attr::ObjectId, got {:?}",
            q.select
        );
    }

    /// Multiple columns: bare alias plus a real attr.  Only the alias column
    /// should become Star; the real attr stays untouched.
    #[test]
    fn bare_alias_mixed_columns_only_alias_rewrites() {
        let q = parse_one("SELECT s, @usedHeapSize FROM java.lang.String s");
        assert_eq!(q.select.len(), 2, "must have 2 columns");
        assert_eq!(q.select[0], SelectItem::Star, "first column (alias) must be Star");
        assert_eq!(
            q.select[1],
            SelectItem::Attr(Attr::UsedHeapSize),
            "second column must be @usedHeapSize"
        );
    }

    /// SUM(s) on an object alias is nonsensical but must not crash; arg rewrites to Star.
    #[test]
    fn bare_alias_in_sum_arg_rewrites_to_star() {
        let q = parse_one("SELECT SUM(s) FROM java.lang.String s");
        match &q.select[0] {
            SelectItem::Aggregate { func: AggFunc::Sum, arg } => {
                assert_eq!(
                    arg.as_ref(),
                    &SelectItem::Star,
                    "SUM(s) arg must rewrite to Star, got {:?}",
                    arg
                );
            }
            other => panic!("expected SUM aggregate, got {other:?}"),
        }
    }

    // ============================================================
    // Group N — numeric-literal grammar (hex/octal/long/char/float suffixes)
    // ============================================================

    #[test]
    fn lex_numeric_literal_forms() {
        use super::Token::*;
        let toks = |s: &str| {
            super::tokenize_spanned(s)
                .unwrap()
                .into_iter()
                .map(|(t, _)| t)
                .collect::<Vec<_>>()
        };
        assert_eq!(toks("100"), vec![Int(100)]);
        assert_eq!(toks("100L"), vec![Int(100)]);
        assert_eq!(toks("100l"), vec![Int(100)]);
        assert_eq!(toks("0xFF"), vec![Int(255)]);
        assert_eq!(toks("0Xff"), vec![Int(255)]);
        assert_eq!(toks("0xFFL"), vec![Int(255)]);
        assert_eq!(toks("0144"), vec![Int(100)]);
        assert_eq!(toks("0144L"), vec![Int(100)]);
        assert_eq!(toks("0"), vec![Int(0)]);
        assert_eq!(toks("08"), vec![Int(8)]); // lenient: not octal, plain decimal
        assert_eq!(toks("'a'"), vec![Int(97)]);
        assert_eq!(toks("1.5"), vec![Float(1.5)]);
        assert_eq!(toks("7."), vec![Float(7.0)]);
        assert_eq!(toks("1.5F"), vec![Float(1.5)]);
        assert_eq!(toks("2.0D"), vec![Float(2.0)]);
        assert_eq!(toks("5F"), vec![Float(5.0)]);
        assert_eq!(toks("5D"), vec![Float(5.0)]);
        assert_eq!(toks("1e5"), vec![Float(100000.0)]);
        assert_eq!(toks("1.5e-3"), vec![Float(0.0015)]);
        assert_eq!(toks("2E+2"), vec![Float(200.0)]);
    }

    #[test]
    fn lex_numeric_literal_errors() {
        assert!(super::tokenize_spanned("0xFFFFFFFFFFFFFFFFF").is_err()); // overflow
        assert!(super::tokenize_spanned("''").is_err()); // empty char
        assert!(super::tokenize_spanned("'ab'").is_err()); // multi-char
        assert!(super::tokenize_spanned("0xZZ").is_ok()); // 0 int + xZZ ident, not an error
    }

    #[test]
    fn lex_high_bit_address_roundtrips() {
        use super::Token::*;
        let toks = |s: &str| {
            super::tokenize_spanned(s)
                .unwrap()
                .into_iter()
                .map(|(t, _)| t)
                .collect::<Vec<_>>()
        };

        // High-bit address (> i64::MAX) lexes to one Int whose bits are the address.
        let t = toks("0xffff800012345678");
        assert_eq!(t.len(), 1);
        match t[0] {
            Int(x) => assert_eq!(x as u64, 0xffff_8000_1234_5678u64),
            ref other => panic!("expected Int, got {other:?}"),
        }

        // u64::MAX round-trips to -1i64 and back.
        let t = toks("0xffffffffffffffff");
        assert_eq!(t.len(), 1);
        match t[0] {
            Int(x) => assert_eq!(x as u64, u64::MAX),
            ref other => panic!("expected Int, got {other:?}"),
        }

        // Full FROM query with a high-bit address resolves to the exact address.
        assert_eq!(
            super::parse("SELECT * FROM OBJECTS 0xffff800012345678")
                .unwrap()
                .from,
            super::FromSource::Object(0xffff_8000_1234_5678)
        );

        // A literal beyond u64::MAX still errors.
        assert!(super::tokenize_spanned("999999999999999999999999999").is_err());
    }

    #[test]
    fn numeric_literals_in_arithmetic() {
        assert!(super::parse("SELECT 0xFF + 1 FROM java.lang.String").is_ok());
        assert!(super::parse("SELECT 2 * 1.5D FROM java.lang.String").is_ok());
        assert!(super::parse("SELECT -0144 FROM java.lang.String").is_ok());
    }

    #[test]
    fn parse_at_name_attribute() {
        assert!(super::parse("SELECT @name FROM java.lang.Thread").is_ok());
        assert!(super::parse(r#"SELECT * FROM java.lang.Thread WHERE @name = "java.lang.Thread""#)
            .is_ok());
    }

    #[test]
    fn parse_at_name_aliases_displayname() {
        // `@name` must produce the SAME query AST as `@displayName`.
        let by_name = super::parse("SELECT @name FROM java.lang.Thread").unwrap();
        let by_display = super::parse("SELECT @displayName FROM java.lang.Thread").unwrap();
        assert_eq!(by_name, by_display);
    }

    #[test]
    fn parse_and_eval_tohex() {
        assert!(super::parse("SELECT toHex(@objectAddress) FROM java.lang.Thread LIMIT 1").is_ok());
        assert!(super::parse("SELECT toHex(255) FROM java.lang.Thread").is_ok());
    }

    // ============================================================
    // Group D1 — method-call postfix syntax: receiver.name(args)
    // ============================================================

    #[test]
    fn parse_method_postfix() {
        assert!(super::parse("SELECT s.getName() FROM java.lang.Thread s").is_ok());
        assert!(super::parse("SELECT i.intValue() FROM java.lang.Integer i").is_ok());
        assert!(super::parse("SELECT a.get(0) FROM java.util.ArrayList a").is_ok());
        assert!(super::parse("SELECT s.value.@length FROM java.lang.String s").is_ok());
        assert!(super::parse("SELECT i.intValue() * 2 FROM java.lang.Integer i").is_ok());
    }

    #[test]
    fn parse_method_postfix_ast_shape() {
        // s.getName() → Expr::Method { receiver: Attr::Field("s"), name: "getName", args: [] }
        let q = super::parse("SELECT s.getName() FROM java.lang.Thread s").unwrap();
        match &q.select[0] {
            SelectItem::Expr(e) => match e.as_ref() {
                Expr::Method { receiver, name, args } => {
                    assert!(
                        matches!(receiver.as_ref(), Expr::Attr(Attr::Field(f)) if f == "s"),
                        "expected Attr::Field(\"s\"), got {receiver:?}"
                    );
                    assert_eq!(name, "getName");
                    assert!(args.is_empty(), "expected no args, got {args:?}");
                }
                other => panic!("expected Expr::Method, got {other:?}"),
            },
            other => panic!("expected SelectItem::Expr, got {other:?}"),
        }

        // a.get(0) → Expr::Method { receiver: Attr::Field("a"), name: "get", args: [Lit(Int(0))] }
        let q = super::parse("SELECT a.get(0) FROM java.util.ArrayList a").unwrap();
        match &q.select[0] {
            SelectItem::Expr(e) => match e.as_ref() {
                Expr::Method { receiver, name, args } => {
                    assert!(
                        matches!(receiver.as_ref(), Expr::Attr(Attr::Field(f)) if f == "a"),
                        "expected Attr::Field(\"a\"), got {receiver:?}"
                    );
                    assert_eq!(name, "get");
                    assert_eq!(args.len(), 1, "expected 1 arg, got {args:?}");
                    assert!(
                        matches!(&args[0], Expr::Lit(Value::Int(0))),
                        "expected Int(0), got {:?}",
                        &args[0]
                    );
                }
                other => panic!("expected Expr::Method, got {other:?}"),
            },
            other => panic!("expected SelectItem::Expr, got {other:?}"),
        }
    }

    #[test]
    fn parse_backing_array_attrs() {
        assert!(super::parse("SELECT @valueArray FROM java.lang.String").is_ok());
        assert!(super::parse("SELECT @referenceArray FROM java.util.ArrayList").is_ok());
    }

    // ============================================================
    // D4b — getKey()/getValue() lower to key/value ref-hops projecting
    // the resolved object's @objectAddress (identity).
    // ============================================================

    #[test]
    fn getkey_lowers_to_key_refpath_objectaddress_tail() {
        // `e.getKey()` normalizes to a one-hop RefPath: hops=["key"] (alias
        // stripped), tail=@objectAddress, projection-only. It must NOT stay an
        // Expr::Method — it rides the RefWalk late-resolution pipeline.
        let q = super::parse("SELECT e.getKey() FROM java.util.HashMap$Node e").unwrap();
        match &q.select[0] {
            SelectItem::Attr(Attr::RefPath { hops, tail, role }) => {
                assert_eq!(hops, &vec!["key".to_string()], "expected single 'key' hop");
                assert_eq!(
                    tail.as_ref(),
                    &Attr::ObjectAddress,
                    "getKey() tail must project the resolved object's address"
                );
                assert_eq!(*role, RefRole::ProjectionOnly);
            }
            other => panic!("expected SelectItem::Attr(RefPath), got {other:?}"),
        }
    }

    #[test]
    fn getvalue_lowers_to_value_refpath_objectaddress_tail() {
        let q = super::parse("SELECT e.getValue() FROM java.util.HashMap$Node e").unwrap();
        match &q.select[0] {
            SelectItem::Attr(Attr::RefPath { hops, tail, .. }) => {
                assert_eq!(hops, &vec!["value".to_string()]);
                assert_eq!(tail.as_ref(), &Attr::ObjectAddress);
            }
            other => panic!("expected SelectItem::Attr(RefPath), got {other:?}"),
        }
    }

    #[test]
    fn non_refhop_methods_are_not_lowered() {
        // getName()/intValue()/size() are scan-time emulated methods (D2/D3) and
        // must remain Expr::Method — they must NOT be lowered to a RefPath.
        for oql in [
            "SELECT s.getName() FROM java.lang.Thread s",
            "SELECT i.intValue() FROM java.lang.Integer i",
            "SELECT c.size() FROM java.util.ArrayList c",
        ] {
            let q = super::parse(oql).unwrap();
            assert!(
                matches!(&q.select[0], SelectItem::Expr(e) if matches!(e.as_ref(), Expr::Method { .. })),
                "method in `{oql}` was unexpectedly lowered away from Expr::Method: {:?}",
                &q.select[0]
            );
        }
    }

    #[test]
    fn getkey_with_args_is_not_lowered() {
        // Only ZERO-arg getKey/getValue lower. A getKey(x) (unusual, but guard it)
        // stays an Expr::Method and flows through scan-time dispatch.
        let q = super::parse("SELECT e.getKey(1) FROM java.util.HashMap$Node e").unwrap();
        assert!(
            matches!(&q.select[0], SelectItem::Expr(e) if matches!(e.as_ref(), Expr::Method { .. })),
            "getKey(1) with an arg must not lower: {:?}",
            &q.select[0]
        );
    }
}
