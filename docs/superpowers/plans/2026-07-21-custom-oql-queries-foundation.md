# Custom OQL Queries — Foundation Slice Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an end-to-end MAT-style OQL query feature covering histogram-only and single-scan field/class queries (parse → plan → execute during pass2 → render into report + interactive REPL), producing genuinely useful working software.

**Architecture:** A new `src/query/` module holds a hand-written parser (AST), a needs-analysis planner (per-need cost gating), and an executor that runs via a per-object callback hook (`ObjectVisitor`) threaded into `pass2::scan_heap_2a`. Results attach to the `Report` and render in md/html/json. A `query` subcommand runs an interactive REPL with `!plan`/`!explain`.

**Tech Stack:** Rust 2024, clap (subcommands), serde (result model), existing pass2/report machinery. No new runtime dependencies in this slice (regex/`LIKE` and edge/dominator/retained/UNION are deferred to later slices).

**Scope — this slice:**
- Grammar: `SELECT [DISTINCT] <list> FROM [INSTANCEOF] <class-spec> [alias] [WHERE <pred>]`, plus `COUNT/SUM/MIN/MAX/AVG` histogram aggregates.
- Attributes: `@objectId`, `@objectAddress`, `@usedHeapSize`, `@displayName`, `@length` (arrays), named scalar/String fields, `classof(x)`.
- WHERE: comparisons (`= != < <= > >=`), `AND`/`OR`/`NOT`, parentheses, string/number/bool literals, `INSTANCEOF`.
- Execution: histogram-only (P4 aggregates) and single-scan (P1 field/class filter with early WHERE + LIMIT, cheapest-predicate-first).

**Deferred to later slices (explicitly rejected with a clear message here):** ref-hop paths (`RefPath`), `@retainedHeapSize`, `dominators()`/`AS RETAINED SET`, `inbounds/outbounds/path`, `UNION`, `LIKE` regex, MAT differential oracle.

**Spec:** `docs/superpowers/specs/2026-07-21-custom-oql-queries-design.md`

---

## File Structure

- `src/query/mod.rs` — module root; `pub mod` declarations; `QueryError` type; the `ObjectVisitor` trait (implemented by the executors, threaded into `pass2`).
- `src/query/ast.rs` — the parsed AST types (`Query`, `SelectItem`, `Predicate`, `Value`, `ClassSpec`, `Attr`, `AggFunc`, `CompareOp`).
- `src/query/parse.rs` — hand-written recursive-descent + Pratt parser: text → `Query` AST.
- `src/query/plan.rs` — `QueryNeeds`, `QueryPlan`, `StageKind`, `Conjunct`/`PredCost`; needs analysis; rejection of deferred constructs; `QueryPlan::explain()`.
- `src/query/model.rs` — serde result types (`QueryResult`, `QueryValue`, `QueryColumn`) attached to `Report`.
- `src/query/execute.rs` — `ClassResolver` trait, `SingleScanExecutor` (implements `ObjectVisitor`); field decode via its own `read_be`/`decode_field` over the public `sizing::field_offset`; shared `class_name_matches`/`column_name` helpers; produces `QueryResult`.
- `src/query/histogram.rs` — `ClassSummary`, `run_histogram`: aggregate-only queries answered from per-class count/shallow totals (no rescan).
- `src/query/run.rs` — integration seam: `LiveResolver` (over live `class_map`/`strings`), `ScanDriver` (fans one visitor pass to N executors), `run_single_dump` (one-shot pipeline for the REPL/subcommand).
- `src/query/repl.rs` — interactive REPL: reads stdin lines, dispatches `!plan`/`!explain`/`!help`/`!quit`, runs queries.
- `src/pass2/mod.rs` — MODIFY: add optional `Option<&mut dyn ObjectVisitor>` param threaded into `scan_heap_2a`; call it at the INSTANCE_DUMP hook site (line ~989); build `LiveResolver`+executors before `class_map`/`strings` drop; add `query_results` to the returned tuple.
- `src/main.rs` — MODIFY: add `mod query;`; `Cmd::Query` subcommand + `--query`/`--query-file` analyze flags; parse+plan queries in `run`; attach results to `Report.queries`.
- `src/report/model.rs` — MODIFY: add `pub queries: Vec<QueryResult>` field to `Report`.
- `src/report/render_md.rs`, `src/html.rs` — MODIFY: render a "Custom Queries" section.
- `tests/query_cli.rs` — new integration test driving the built binary (matches this repo's convention: all `tests/*.rs` use `CARGO_BIN_EXE_hprof-analyzer`, there is NO library target and NO `hprof_analyzer::` import path).

**Testing convention (IMPORTANT — this repo is binary-only):** Unit tests for the parser, planner, executor, and histogram executor live as `#[cfg(test)] mod tests { ... }` blocks *inside* the source files (`src/query/parse.rs`, `src/query/plan.rs`, `src/query/execute.rs`, `src/query/histogram.rs`) — exactly like `src/vbyte.rs` and `src/types.rs` do. These are run with `cargo test`. End-to-end behavior (REPL, `--query`, report rendering) is tested in `tests/query_cli.rs` by spawning the built binary, mirroring `tests/cli_unified.rs`. Do NOT add a `[lib]` target or `src/lib.rs` — no existing test imports the crate as a library.

---

## Component 1 — AST + Parser (`src/query/ast.rs`, `src/query/parse.rs`)

### Task 1: Create the query module skeleton and AST types

**Files:**
- Create: `src/query/mod.rs`
- Create: `src/query/ast.rs`
- Modify: `src/main.rs` (add `mod query;` alongside other `mod` declarations near line 14-37)

- [ ] **Step 1: Add the module declaration**

In `src/main.rs`, add after `mod progress;` (line 28):

```rust
mod query;
```

- [ ] **Step 2: Write the AST types**

Create `src/query/ast.rs`:

```rust
//! Parsed OQL query AST. Mirrors the Eclipse MAT OQL surface this analyzer
//! supports; unsupported constructs are rejected in the planner, not here.

/// A parsed query. `union` is reserved for a later slice and always empty here.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub distinct: bool,
    pub select: Vec<SelectItem>,
    pub from: ClassSpec,
    pub alias: Option<String>,
    pub where_: Option<Predicate>,
    pub limit: Option<u64>,
}

/// One projected column.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectItem {
    /// `*` — the object itself (rendered as its display name / ref).
    Star,
    /// An attribute or named field of the FROM alias, e.g. `@usedHeapSize`, `name`.
    Attr(Attr),
    /// An aggregate over all matched instances, e.g. `COUNT(*)`, `SUM(@usedHeapSize)`.
    Aggregate { func: AggFunc, arg: Box<SelectItem> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc { Count, Sum, Min, Max, Avg }

/// An attribute reference. `@`-prefixed built-ins plus bare named fields.
#[derive(Debug, Clone, PartialEq)]
pub enum Attr {
    ObjectId,
    ObjectAddress,
    UsedHeapSize,
    DisplayName,
    Length,
    /// `classof(x)` — the runtime class name.
    ClassOf,
    /// A bare instance field name, e.g. `count`, `value`.
    Field(String),
}

/// The FROM clause target.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassSpec {
    /// true for `FROM INSTANCEOF C` (subclasses included), false for `FROM C`.
    pub instanceof: bool,
    /// The class name as written, e.g. `java.lang.String` or `com.acme.*`.
    pub class_name: String,
}

/// A WHERE predicate tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
    Not(Box<Predicate>),
    Compare { lhs: Attr, op: CompareOp, rhs: Value },
    /// `x INSTANCEOF C`
    InstanceOf(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp { Eq, Ne, Lt, Le, Gt, Ge }

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
}
```

- [ ] **Step 3: Write the module root**

Create `src/query/mod.rs`:

```rust
//! MAT-style OQL query support: parse, plan, execute against pipeline data,
//! and render into the report. This slice covers histogram-only and
//! single-scan field/class queries; graph/retained/edge primitives are
//! rejected by the planner with a clear message (see the design spec).

pub mod ast;
pub mod parse;

use std::fmt;

/// A user-facing query error, surfaced verbatim in results and the REPL.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryError(pub String);

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for QueryError {}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build 2>&1 | tail -20`
Expected: builds (warnings about unused `parse` module are fine until Task 2).

- [ ] **Step 5: Commit**

```bash
git add src/query/mod.rs src/query/ast.rs src/main.rs
git commit -m "feat(query): add OQL AST types and query module skeleton"
```

### Task 2: Tokenizer

**Files:**
- Create: `src/query/parse.rs` (with an in-file `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing tokenizer test (in-file)**

Create `src/query/parse.rs` with ONLY the test module first so it fails to compile against missing items:

```rust
//! Hand-written tokenizer + recursive-descent/Pratt parser for the supported
//! OQL subset. No parser-generator dependency; the grammar is small and fixed.

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
        assert_eq!(toks[5], Token::Str("foo".into()));
    }
}
```

Also register the module: in `src/query/mod.rs` the line `pub mod parse;` already exists from Task 1.

- [ ] **Step 2: Verify it fails to compile**

Run: `cargo test parse:: 2>&1 | tail -20`
Expected: FAIL — `tokenize`/`Token` not found.

- [ ] **Step 3: Implement the tokenizer**

Prepend the implementation ABOVE the `#[cfg(test)] mod tests` block in `src/query/parse.rs`:

```rust
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
```

- [ ] **Step 4: Run tokenizer tests**

Run: `cargo test parse:: 2>&1 | tail -20`
Expected: PASS (both `tokenize_*` tests).

- [ ] **Step 5: Commit**

```bash
git add src/query/parse.rs
git commit -m "feat(query): add OQL tokenizer"
```

### Task 3: Recursive-descent + Pratt parser (tokens → `Query`)

**Files:**
- Modify: `src/query/parse.rs` (add parser above the test module; extend tests)

- [ ] **Step 1: Write failing parser tests (in-file)**

Add these tests inside the existing `#[cfg(test)] mod tests` block in `src/query/parse.rs`:

```rust
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
```

- [ ] **Step 2: Verify it fails to compile**

Run: `cargo test parse:: 2>&1 | tail -20`
Expected: FAIL — `parse` fn not found.

- [ ] **Step 3: Implement the parser**

Add above the test module in `src/query/parse.rs`:

```rust
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
        if matches!(self.bump(), Some(Token::LParen)) { Ok(()) }
        else { Err(QueryError("expected `(`".into())) }
    }
    fn expect_rparen(&mut self) -> Result<(), QueryError> {
        if matches!(self.bump(), Some(Token::RParen)) { Ok(()) }
        else { Err(QueryError("expected `)`".into())) }
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
```

- [ ] **Step 4: Run parser tests**

Run: `cargo test parse:: 2>&1 | tail -25`
Expected: PASS — all `parse_*` and `tokenize_*` tests green.

- [ ] **Step 5: Commit**

```bash
git add src/query/parse.rs
git commit -m "feat(query): add recursive-descent OQL parser (SELECT/FROM/WHERE/LIMIT/aggregates)"
```

---

## Component 2 — Planner (`src/query/plan.rs`)

The planner walks the AST to derive `QueryNeeds` (per-need cost gating), rejects
any construct deferred to a later slice (naming it), and classifies the query
into a `StageKind` (`HistogramOnly` or `SingleScan`). It also orders WHERE
conjuncts cheapest-first and marks projection-only attributes.

### Task 4: QueryNeeds + rejection of deferred constructs

**Files:**
- Create: `src/query/plan.rs` (with in-file tests)
- Modify: `src/query/mod.rs` (add `pub mod plan;`)

- [ ] **Step 1: Register the module**

In `src/query/mod.rs`, add after `pub mod parse;`:

```rust
pub mod plan;
```

- [ ] **Step 2: Write failing planner tests (in-file)**

Create `src/query/plan.rs` with the test module first:

```rust
//! Needs analysis + planning for the supported OQL subset. Cost is per-need:
//! each flag arms exactly one piece of machinery. Deferred constructs are
//! rejected here (not in the parser) with a message naming the construct.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse::parse;

    #[test]
    fn histogram_only_needs() {
        let plan = plan_query(&parse("SELECT COUNT(*) FROM java.lang.String").unwrap()).unwrap();
        assert_eq!(plan.kind, StageKind::HistogramOnly);
        assert!(!plan.needs.instance_scalar);
        assert!(!plan.needs.instance_string);
    }

    #[test]
    fn single_scan_scalar_needs() {
        let plan = plan_query(
            &parse("SELECT @objectId FROM C WHERE count > 3").unwrap(),
        )
        .unwrap();
        assert_eq!(plan.kind, StageKind::SingleScan);
        assert!(plan.needs.instance_scalar);
        assert!(!plan.needs.instance_string);
    }

    #[test]
    fn string_projection_sets_string_need() {
        let plan = plan_query(&parse("SELECT @displayName FROM java.lang.String").unwrap()).unwrap();
        assert!(plan.needs.instance_string);
    }

    #[test]
    fn rejects_retained_heap() {
        let err = plan_query(&parse("SELECT @retainedHeapSize FROM C").unwrap());
        // @retainedHeapSize isn't a known @attr in this slice: parser rejects it.
        // If the parser accepted it, the planner must reject. Either way -> Err.
        assert!(err.is_err());
    }

    #[test]
    fn rejects_distinct_for_now() {
        let err = plan_query(&parse("SELECT DISTINCT * FROM C").unwrap()).unwrap_err();
        assert!(err.0.to_lowercase().contains("distinct"), "got: {}", err.0);
    }

    #[test]
    fn predicates_ordered_cheapest_first() {
        // A String comparison (expensive) and a scalar comparison (cheap):
        // the plan should evaluate the scalar first.
        let plan = plan_query(
            &parse("SELECT * FROM C WHERE name = \"x\" AND count > 1").unwrap(),
        )
        .unwrap();
        // First conjunct in the flattened order must be the scalar `count`.
        assert!(matches!(
            plan.where_terms.first(),
            Some(Conjunct { cost: PredCost::Scalar, .. })
        ));
    }
}
```

- [ ] **Step 3: Verify it fails to compile**

Run: `cargo test plan:: 2>&1 | tail -20`
Expected: FAIL — planner items not found.

- [ ] **Step 4: Implement the planner**

Add above the test module in `src/query/plan.rs`:

```rust
use crate::query::ast::{Attr, Predicate, Query, SelectItem, Value};
use crate::query::QueryError;

/// Per-need cost flags. Each flag independently arms exactly one piece of
/// machinery; an unset flag arms nothing. (Foundation subset — ref/retained/
/// dominator/edge needs are added in later slices.)
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QueryNeeds {
    /// Class-level aggregates only (COUNT/SUM/MIN/MAX/AVG over shallow/instances).
    pub histogram: bool,
    /// Scalar (non-String, non-ref) fields of matched instances must be decoded.
    pub instance_scalar: bool,
    /// String fields / @displayName must be decoded (String backing array).
    pub instance_string: bool,
    /// Runtime class of the row is needed (classof / INSTANCEOF in WHERE).
    pub runtime_type: bool,
}

/// The shape of the plan. Derived from `needs`; drives which executor runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKind {
    /// Answerable from per-class aggregates alone (single P4 pass, no carries).
    HistogramOnly,
    /// A single P1 field/class scan with early WHERE + LIMIT.
    SingleScan,
}

/// Relative evaluation cost of a WHERE conjunct — used to order cheapest-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredCost {
    /// Class-index / INSTANCEOF test — cheapest.
    Type,
    /// Scalar field compare.
    Scalar,
    /// String field decode + compare — most expensive in this slice.
    Str,
}

/// One flattened WHERE conjunct with its precomputed cost class.
#[derive(Debug, Clone, PartialEq)]
pub struct Conjunct {
    pub pred: Predicate,
    pub cost: PredCost,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryPlan {
    pub kind: StageKind,
    pub needs: QueryNeeds,
    /// AND-conjuncts flattened and ordered cheapest-first. OR/NOT subtrees are
    /// kept whole as a single conjunct at their max member cost.
    pub where_terms: Vec<Conjunct>,
    pub limit: Option<u64>,
}

/// Analyze + plan a parsed query, or reject with a message naming the
/// unsupported construct.
pub fn plan_query(q: &Query) -> Result<QueryPlan, QueryError> {
    if q.distinct {
        return Err(QueryError(
            "DISTINCT is not supported in this version (deferred)".into(),
        ));
    }

    let mut needs = QueryNeeds::default();
    let mut is_aggregate = false;

    for item in &q.select {
        match item {
            SelectItem::Aggregate { arg, .. } => {
                is_aggregate = true;
                note_attr_need(arg, &mut needs)?;
            }
            SelectItem::Star => { /* row identity: needs display name at render */ }
            SelectItem::Attr(a) => note_attr_need_attr(a, &mut needs),
        }
    }

    // WHERE contributes needs and is flattened+ordered.
    let mut where_terms = Vec::new();
    if let Some(pred) = &q.where_ {
        collect_pred_needs(pred, &mut needs)?;
        flatten_and(pred.clone(), &mut where_terms);
        where_terms.sort_by_key(|c| pred_cost_rank(c.cost));
    }

    // Aggregates with no per-instance projection/filter are histogram-only.
    let kind = if is_aggregate
        && !needs.instance_scalar
        && !needs.instance_string
        && where_terms.is_empty()
    {
        needs.histogram = true;
        StageKind::HistogramOnly
    } else {
        StageKind::SingleScan
    };

    Ok(QueryPlan { kind, needs, where_terms, limit: q.limit })
}

fn note_attr_need(item: &SelectItem, needs: &mut QueryNeeds) -> Result<(), QueryError> {
    match item {
        SelectItem::Star => Ok(()),
        SelectItem::Attr(a) => { note_attr_need_attr(a, needs); Ok(()) }
        SelectItem::Aggregate { .. } => {
            Err(QueryError("nested aggregate is not supported".into()))
        }
    }
}

fn note_attr_need_attr(a: &Attr, needs: &mut QueryNeeds) {
    match a {
        Attr::DisplayName => needs.instance_string = true,
        Attr::ClassOf => needs.runtime_type = true,
        Attr::Field(_) => {
            // A named field may be scalar or String; the executor resolves the
            // type from the schema. Arm scalar decode; String decode is armed
            // lazily when the resolved type is Object/String.
            needs.instance_scalar = true;
        }
        // @objectId/@objectAddress/@usedHeapSize/@length need no field decode.
        _ => {}
    }
}

fn collect_pred_needs(pred: &Predicate, needs: &mut QueryNeeds) -> Result<(), QueryError> {
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            collect_pred_needs(a, needs)?;
            collect_pred_needs(b, needs)
        }
        Predicate::Not(a) => collect_pred_needs(a, needs),
        Predicate::InstanceOf(_) => { needs.runtime_type = true; Ok(()) }
        Predicate::Compare { lhs, rhs, .. } => {
            match lhs {
                Attr::Field(_) => {
                    if matches!(rhs, Value::Str(_)) {
                        needs.instance_string = true;
                    } else {
                        needs.instance_scalar = true;
                    }
                }
                Attr::DisplayName => needs.instance_string = true,
                Attr::ClassOf => needs.runtime_type = true,
                _ => {}
            }
            Ok(())
        }
    }
}

/// Flatten a right-leaning AND tree into conjuncts. OR/NOT/Compare/InstanceOf
/// are kept as single conjuncts.
fn flatten_and(pred: Predicate, out: &mut Vec<Conjunct>) {
    match pred {
        Predicate::And(a, b) => {
            flatten_and(*a, out);
            flatten_and(*b, out);
        }
        other => {
            let cost = pred_cost(&other);
            out.push(Conjunct { pred: other, cost });
        }
    }
}

fn pred_cost(pred: &Predicate) -> PredCost {
    match pred {
        Predicate::InstanceOf(_) => PredCost::Type,
        Predicate::Not(a) => pred_cost(a),
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            pred_cost(a).max_cost(pred_cost(b))
        }
        Predicate::Compare { lhs, rhs, .. } => match lhs {
            Attr::Field(_) if matches!(rhs, Value::Str(_)) => PredCost::Str,
            Attr::DisplayName => PredCost::Str,
            Attr::ClassOf => PredCost::Type,
            _ => PredCost::Scalar,
        },
    }
}

impl PredCost {
    fn max_cost(self, other: PredCost) -> PredCost {
        if pred_cost_rank(self) >= pred_cost_rank(other) { self } else { other }
    }
}

fn pred_cost_rank(c: PredCost) -> u8 {
    match c {
        PredCost::Type => 0,
        PredCost::Scalar => 1,
        PredCost::Str => 2,
    }
}
```

- [ ] **Step 5: Run planner tests**

Run: `cargo test plan:: 2>&1 | tail -25`
Expected: PASS — all `*_needs`, `rejects_*`, and ordering tests green.

- [ ] **Step 6: Commit**

```bash
git add src/query/plan.rs src/query/mod.rs
git commit -m "feat(query): add needs-analysis planner with per-need gating and cheapest-first WHERE ordering"
```

### Task 5: `!explain` rendering of a plan

**Files:**
- Modify: `src/query/plan.rs` (add `explain` fn + test)

- [ ] **Step 1: Write the failing test (in-file)**

Add to the `#[cfg(test)] mod tests` block in `src/query/plan.rs`:

```rust
    #[test]
    fn explain_lists_kind_and_needs() {
        let plan = plan_query(&parse("SELECT @objectId FROM C WHERE count > 3").unwrap()).unwrap();
        let text = plan.explain();
        assert!(text.contains("SingleScan"));
        assert!(text.contains("instance_scalar"));
        assert!(text.contains("cheapest-first"));
    }
```

- [ ] **Step 2: Verify it fails**

Run: `cargo test plan::tests::explain 2>&1 | tail -15`
Expected: FAIL — no `explain` method.

- [ ] **Step 3: Implement `explain`**

Add to `impl QueryPlan` (create the impl block above the tests) in `src/query/plan.rs`:

```rust
impl QueryPlan {
    /// Human-readable plan summary for `!explain` / `!plan`.
    pub fn explain(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("stage: {:?}\n", self.kind));
        let mut armed = Vec::new();
        if self.needs.histogram { armed.push("histogram"); }
        if self.needs.instance_scalar { armed.push("instance_scalar"); }
        if self.needs.instance_string { armed.push("instance_string"); }
        if self.needs.runtime_type { armed.push("runtime_type"); }
        s.push_str(&format!(
            "needs (armed): {}\n",
            if armed.is_empty() { "none".into() } else { armed.join(", ") }
        ));
        if let Some(n) = self.limit {
            s.push_str(&format!("limit: {n}\n"));
        }
        if !self.where_terms.is_empty() {
            s.push_str("where (cheapest-first):\n");
            for c in &self.where_terms {
                s.push_str(&format!("  [{:?}] {:?}\n", c.cost, c.pred));
            }
        }
        s
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test plan::tests::explain 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/query/plan.rs
git commit -m "feat(query): add plan explain() for !plan/!explain REPL commands"
```

---

## Component 3 — Result model, pass2 hook, executor

### Task 6: Query result model (serde types on the Report)

**Files:**
- Create: `src/query/model.rs` (with in-file tests)
- Modify: `src/query/mod.rs` (add `pub mod model;`)

- [ ] **Step 1: Register the module**

In `src/query/mod.rs`, add after `pub mod plan;`:

```rust
pub mod model;
```

- [ ] **Step 2: Write the failing test (in-file)**

Create `src/query/model.rs`:

```rust
//! Serde-serializable query results attached to the Report and rendered in
//! md/html/json. Mirrors the spec's QueryResult/QueryValue shapes.

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_serializes_stably() {
        let r = QueryResult {
            name: "q1".into(),
            oql: "SELECT COUNT(*) FROM C".into(),
            columns: vec![QueryColumn { name: "COUNT(*)".into() }],
            rows: vec![vec![QueryValue::Int(42)]],
            row_count: 1,
            truncated: false,
            error: None,
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"row_count\":1"));
        assert!(j.contains("\"truncated\":false"));
        let back: QueryResult = serde_json::from_str(&j).unwrap();
        assert_eq!(back, r);
    }
}
```

- [ ] **Step 3: Verify it fails**

Run: `cargo test model:: 2>&1 | tail -15`
Expected: FAIL — types not found.

- [ ] **Step 4: Implement the model**

Add above the test module in `src/query/model.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryColumn {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "v", rename_all = "snake_case")]
pub enum QueryValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A reference to a heap object: dense index + its class name for display.
    ObjRef { index: u64, class: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    /// Query label (e.g. "q1" or a user-supplied name).
    pub name: String,
    /// The original OQL text.
    pub oql: String,
    pub columns: Vec<QueryColumn>,
    pub rows: Vec<Vec<QueryValue>>,
    pub row_count: u64,
    /// True if a cap was hit and rows are a bounded sample.
    pub truncated: bool,
    /// Set (with rows empty) when the query failed to parse/plan/execute.
    pub error: Option<String>,
}
```

Note: `QueryValue::Float` uses `f64`; serde handles it. `PartialEq` on `f64` is
fine for round-trip tests (no NaN literals in results).

- [ ] **Step 5: Run the test**

Run: `cargo test model:: 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/query/model.rs src/query/mod.rs
git commit -m "feat(query): add serde QueryResult/QueryValue result model"
```

### Task 7: Add `queries` field to Report and render nothing yet (plumbing)

**Files:**
- Modify: `src/report/model.rs` (add field to `Report`)
- Modify: `src/report/build.rs` (initialize the field empty in `build_model`)
- Modify: `schema/report.schema.json` is auto-generated — regenerate after.

- [ ] **Step 1: Add the field to the Report struct**

In `src/report/model.rs`, find `pub struct Report {` (around line 1279) and add as the LAST field (before the closing brace), so existing field order/serialization of prior fields is unchanged:

```rust
    /// Custom OQL query results (empty unless --query/--query-file was given).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queries: Vec<crate::query::model::QueryResult>,
```

- [ ] **Step 2: Initialize it in build_model**

In `src/report/build.rs`, find where the `Report { ... }` literal is constructed in `build_model` (after the section builders). Add the field initializer:

```rust
        queries: Vec::new(),
```

- [ ] **Step 3: Build**

Run: `cargo build 2>&1 | tail -20`
Expected: builds. If `build_model` constructs `Report` via `..Default::default()` or a helper, add the field there instead — inspect the actual literal.

- [ ] **Step 4: Regenerate the JSON schema**

Run: `cargo run -- dev emit-schema > schema/report.schema.json 2>/dev/null; git diff --stat schema/report.schema.json`
Expected: schema now includes an optional `queries` array. (If the dev subcommand name differs, check `DevCmd` in main.rs — it was `EmitSchema`.)

- [ ] **Step 5: Run existing report tests to confirm no regression**

Run: `cargo test 2>&1 | tail -25`
Expected: existing tests still pass (queries defaults to empty, `skip_serializing_if` keeps JSON output identical for query-less runs).

- [ ] **Step 6: Commit**

```bash
git add src/report/model.rs src/report/build.rs schema/report.schema.json
git commit -m "feat(report): add optional queries field to Report model"
```

### Task 8: Add an `ObjectVisitor` hook to `scan_heap_2a`

**Files:**
- Modify: `src/query/mod.rs` (define the `ObjectVisitor` trait)
- Modify: `src/pass2/mod.rs` (thread `Option<&mut dyn ObjectVisitor>` through `build` → `scan_heap_2a`; call at the INSTANCE_DUMP hook site)

This is the one hot-loop change. It is a no-op when no query is present (the
`Option` is `None`), so query-less runs are byte-for-byte and RSS-identical.

- [ ] **Step 1: Define the visitor trait**

In `src/query/mod.rs`, add:

```rust
/// Per-object callback invoked once per INSTANCE_DUMP during the pass2 2a scan,
/// while the raw instance blob and schema tables are still live. Implementors
/// accumulate query matches. Called only when a query is active.
pub trait ObjectVisitor {
    /// `src_idx` is the dense object index; `class_id` the class-object address;
    /// `blob` the raw big-endian instance field bytes.
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]);
}
```

- [ ] **Step 2: Write a failing test proving the hook fires**

Add an in-file test module at the bottom of `src/pass2/mod.rs` (or extend an
existing `#[cfg(test)] mod tests`). This test drives the built binary end-to-end
later; here we assert the wiring compiles and a counting visitor is invoked. Add:

```rust
#[cfg(test)]
mod visitor_hook_tests {
    // A minimal visitor that counts instance callbacks.
    struct Counter { n: usize }
    impl crate::query::ObjectVisitor for Counter {
        fn visit_instance(&mut self, _src_idx: usize, _class_id: u64, _blob: &[u8]) {
            self.n += 1;
        }
    }

    #[test]
    fn counter_visitor_type_checks() {
        // Compile-time proof the trait object is usable as the scan expects.
        let mut c = Counter { n: 0 };
        let _dyn: &mut dyn crate::query::ObjectVisitor = &mut c;
        c.visit_instance(0, 0, &[]);
        assert_eq!(c.n, 1);
    }
}
```

- [ ] **Step 3: Verify it fails to compile**

Run: `cargo test visitor_hook 2>&1 | tail -15`
Expected: FAIL — `ObjectVisitor` not found in `crate::query` until Step 1 lands (if Step 1 already applied, this compiles and passes trivially; that's fine — proceed).

- [ ] **Step 4: Thread the visitor through `scan_heap_2a`**

In `src/pass2/mod.rs`, change the `scan_heap_2a` signature (line 904) to add a
trailing param:

```rust
    fn scan_heap_2a(
        r: &mut HprofReader,
        id_size: u8,
        mut remaining: u64,
        id_map: &crate::id_map::IdMap,
        class_addr_to_hist: &HashMap<u64, u32>,
        field_plans_dense: &[FieldPlan],
        out_degree: &mut Vec<u32>,
        in_degree: &mut Vec<u32>,
        scratch: &mut Vec<u8>,
        visitor: Option<&mut dyn crate::query::ObjectVisitor>,
    ) -> io::Result<()> {
```

At the INSTANCE_DUMP hook site (after `let src_idx = ...` resolves, ~line 989,
BEFORE the `edge_if_valid!` line), add:

```rust
                    if let Some(v) = visitor.as_deref_mut() {
                        v.visit_instance(src_idx, class_id, scratch);
                    }
```

Because `visitor` is moved into the loop across iterations, change the parameter
binding to `mut visitor: Option<&mut dyn crate::query::ObjectVisitor>` and use
`visitor.as_deref_mut()` (which reborrows without consuming). Confirm the
`as_deref_mut` reborrow compiles inside the loop.

- [ ] **Step 5: Update the caller in `build`**

`scan_heap_2a` is called at line 345. Thread an `Option` param through `build`.
Add a parameter to `Pass2::build` (line 58) AFTER `opts`:

```rust
        visitor: Option<&mut dyn crate::query::ObjectVisitor>,
```

Then at the call site (line 345), pass it. Since the scan loop may call
`scan_heap_2a` multiple times (multiple HEAP_DUMP_SEGMENTs), hold the visitor in
a `let mut visitor = visitor;` above the loop and pass `visitor.as_deref_mut()`
each call:

```rust
                    tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                        Self::scan_heap_2a(
                            &mut r,
                            id_size,
                            length,
                            &p1.id_map,
                            &class_addr_to_hist,
                            &field_plans_dense,
                            &mut out_degree,
                            &mut in_degree,
                            &mut scratch,
                            visitor.as_deref_mut(),
                        )?;
                    }
```

- [ ] **Step 6: Fix all other `Pass2::build` call sites**

Run: `grep -rn "Pass2::build\|pass2::Pass2::build" src/ tests/`
Expected: find every caller (notably `src/main.rs:764`). Add `None` as the new
trailing argument at each existing call site so behavior is unchanged. Example
in `src/main.rs`:

```rust
        pass2::Pass2::build(input, p1, compress, &opts, None)?
```

- [ ] **Step 7: Build + run the hook test + full suite**

Run: `cargo test visitor_hook 2>&1 | tail -15 && cargo test 2>&1 | tail -25`
Expected: hook test PASS; full suite unchanged (all previously-passing tests
still pass — the `None` visitor is a no-op).

- [ ] **Step 8: Commit**

```bash
git add src/query/mod.rs src/pass2/mod.rs src/main.rs
git commit -m "feat(pass2): add optional per-object ObjectVisitor hook to 2a scan (no-op when absent)"
```

### Task 9a: SingleScan executor — class matching + object-identity projection

**Files:**
- Create: `src/query/execute.rs` (with in-file tests)
- Modify: `src/query/mod.rs` (add `pub mod execute;`)

The executor is constructed with borrows of the schema tables that are live
during the 2a scan. It implements `ObjectVisitor`: for each instance it checks
class match, evaluates WHERE (Task 9b), and if it passes and the LIMIT is not
reached, appends a row.

- [ ] **Step 1: Register the module**

In `src/query/mod.rs`, add after `pub mod model;`:

```rust
pub mod execute;
```

- [ ] **Step 2: Write failing executor tests (in-file)**

Create `src/query/execute.rs`:

```rust
//! Executor for the supported OQL subset. SingleScanExecutor implements
//! ObjectVisitor and accumulates bounded rows during the pass2 2a scan.
//! HistogramExecutor answers aggregate-only queries from per-class stats.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::*;
    use crate::query::plan::plan_query;

    // A tiny fake schema for unit testing class matching without a real dump.
    fn schema() -> TestSchema {
        TestSchema {
            // class_id -> class name
            names: vec![(10u64, "com.acme.Foo".to_string()), (20, "com.acme.Bar".to_string())]
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn matches_exact_class_and_projects_object_id() {
        let q = crate::query::parse::parse("SELECT @objectId FROM com.acme.Foo").unwrap();
        let plan = plan_query(&q).unwrap();
        let sc = schema();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(3, 10, &[]); // Foo -> match
        ex.visit_instance(4, 20, &[]); // Bar -> no match
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], crate::query::model::QueryValue::Int(3));
    }

    #[test]
    fn respects_limit() {
        let q = crate::query::parse::parse("SELECT @objectId FROM com.acme.Foo LIMIT 1").unwrap();
        let plan = plan_query(&q).unwrap();
        let sc = schema();
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[]);
        ex.visit_instance(2, 10, &[]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert!(res.truncated);
    }
}
```

- [ ] **Step 3: Verify it fails**

Run: `cargo test execute:: 2>&1 | tail -20`
Expected: FAIL — executor/`TestSchema`/`ClassResolver` not found.

- [ ] **Step 4: Implement the schema-resolver abstraction + executor core**

Add above the tests in `src/query/execute.rs`. The executor depends on a small
`ClassResolver` trait so it can be unit-tested with a fake and wired to the real
pass2 tables in Task 10:

```rust
use crate::query::ast::{Attr, Query, SelectItem, Value};
use crate::query::model::{QueryColumn, QueryResult, QueryValue};
use crate::query::plan::QueryPlan;
use crate::query::ObjectVisitor;

/// Abstracts class-name resolution + field-offset lookup so the executor can be
/// unit-tested against a fake and run against the real pass2 schema in prod.
pub trait ClassResolver {
    /// The class name for a class-object address, dot-separated (e.g. java.lang.String).
    fn class_name(&self, class_id: u64) -> Option<&str>;
    /// Resolve a named instance field to (offset, hprof type) for a class.
    /// Returns None if the field is absent. (Implemented over `field_offset`
    /// in the real resolver; the test fake returns None.)
    fn field(&self, _class_id: u64, _name: &str) -> Option<(u32, crate::types::HprofType)> {
        None
    }
    /// The object's address for a dense index (for @objectAddress). Optional.
    fn addr_of(&self, _src_idx: usize) -> Option<u64> {
        None
    }
    /// Shallow size for a dense index (for @usedHeapSize). Optional.
    fn shallow_of(&self, _src_idx: usize) -> Option<u32> {
        None
    }
}

#[cfg(test)]
pub struct TestSchema {
    pub names: std::collections::HashMap<u64, String>,
}

#[cfg(test)]
impl ClassResolver for TestSchema {
    fn class_name(&self, class_id: u64) -> Option<&str> {
        self.names.get(&class_id).map(|s| s.as_str())
    }
}

/// Executes a SingleScan query by visiting matched instances during the 2a scan.
pub struct SingleScanExecutor<'a, R: ClassResolver> {
    query: &'a Query,
    plan: &'a QueryPlan,
    resolver: &'a R,
    rows: Vec<Vec<QueryValue>>,
    matched: u64,
    truncated: bool,
}

impl<'a, R: ClassResolver> SingleScanExecutor<'a, R> {
    pub fn new(query: &'a Query, plan: &'a QueryPlan, resolver: &'a R) -> Self {
        Self { query, plan, resolver, rows: Vec::new(), matched: 0, truncated: false }
    }

    /// Does this instance's class match the FROM clause?
    fn class_matches(&self, class_id: u64) -> bool {
        let want = &self.query.from.class_name;
        match self.resolver.class_name(class_id) {
            None => false,
            Some(name) => class_name_matches(name, want),
            // INSTANCEOF (subclass) matching is added in a later slice; exact +
            // glob only here.
        }
    }

    /// Build one projected row for a matched instance.
    fn project_row(&self, src_idx: usize, class_id: u64, blob: &[u8]) -> Vec<QueryValue> {
        self.query
            .select
            .iter()
            .map(|item| self.project_item(item, src_idx, class_id, blob))
            .collect()
    }

    fn project_item(&self, item: &SelectItem, src_idx: usize, class_id: u64, blob: &[u8]) -> QueryValue {
        match item {
            SelectItem::Star => QueryValue::ObjRef {
                index: src_idx as u64,
                class: self.resolver.class_name(class_id).unwrap_or("?").to_string(),
            },
            SelectItem::Aggregate { .. } => QueryValue::Null, // not reached in SingleScan projection
            SelectItem::Attr(a) => self.project_attr(a, src_idx, class_id, blob),
        }
    }

    fn project_attr(&self, a: &Attr, src_idx: usize, class_id: u64, blob: &[u8]) -> QueryValue {
        match a {
            Attr::ObjectId => QueryValue::Int(src_idx as i64),
            Attr::ObjectAddress => self
                .resolver
                .addr_of(src_idx)
                .map(|x| QueryValue::Int(x as i64))
                .unwrap_or(QueryValue::Null),
            Attr::UsedHeapSize => self
                .resolver
                .shallow_of(src_idx)
                .map(|x| QueryValue::Int(x as i64))
                .unwrap_or(QueryValue::Null),
            Attr::ClassOf | Attr::DisplayName => QueryValue::Str(
                self.resolver.class_name(class_id).unwrap_or("?").to_string(),
            ),
            Attr::Length => QueryValue::Null, // arrays handled in a later slice
            Attr::Field(name) => self.decode_field(class_id, name, blob),
        }
    }

    // Field decode is implemented in Task 9b; stub returns Null for now.
    fn decode_field(&self, _class_id: u64, _name: &str, _blob: &[u8]) -> QueryValue {
        QueryValue::Null
    }

    // WHERE evaluation is implemented in Task 9b; stub matches all for now.
    fn where_passes(&self, _class_id: u64, _blob: &[u8]) -> bool {
        true
    }

    /// Finalize into a QueryResult with column headers.
    pub fn finish(self, name: &str) -> QueryResult {
        let columns = self
            .query
            .select
            .iter()
            .map(|it| QueryColumn { name: column_name(it) })
            .collect();
        QueryResult {
            name: name.to_string(),
            oql: String::new(), // filled by caller (has the source text)
            columns,
            row_count: self.rows.len() as u64,
            rows: self.rows,
            truncated: self.truncated,
            error: None,
        }
    }
}

impl<'a, R: ClassResolver> ObjectVisitor for SingleScanExecutor<'a, R> {
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        if !self.class_matches(class_id) {
            return;
        }
        if !self.where_passes(class_id, blob) {
            return;
        }
        if let Some(limit) = self.plan.limit {
            if self.matched >= limit {
                self.truncated = true;
                return;
            }
        }
        self.matched += 1;
        let row = self.project_row(src_idx, class_id, blob);
        self.rows.push(row);
    }
}

/// Exact match, or trailing `.*` glob (e.g. `com.acme.*`). Accepts both `.` and
/// `/` separators in the stored name (HPROF uses `/`; we normalize to `.`).
pub fn class_name_matches(name_dotted: &str, pattern: &str) -> bool {
    let name = name_dotted.replace('/', ".");
    let pat = pattern.replace('/', ".");
    if let Some(prefix) = pat.strip_suffix(".*") {
        name == prefix || name.starts_with(&format!("{prefix}."))
    } else {
        name == pat
    }
}

/// Column header label for a SELECT item. `pub` so the histogram executor can
/// reuse it for identical labels. Aggregates render as `FUNC(inner)` with the
/// function name upper-cased but the inner attribute left verbatim (so
/// `SUM(@usedHeapSize)` stays lower-cased inside the parens).
pub fn column_name(it: &SelectItem) -> String {
    match it {
        SelectItem::Star => "*".to_string(),
        SelectItem::Attr(a) => attr_name(a),
        SelectItem::Aggregate { func, arg } => {
            let f = format!("{func:?}").to_uppercase();
            format!("{f}({})", column_name(arg))
        }
    }
}

fn attr_name(a: &Attr) -> String {
    match a {
        Attr::ObjectId => "@objectId".into(),
        Attr::ObjectAddress => "@objectAddress".into(),
        Attr::UsedHeapSize => "@usedHeapSize".into(),
        Attr::DisplayName => "@displayName".into(),
        Attr::Length => "@length".into(),
        Attr::ClassOf => "classof".into(),
        Attr::Field(f) => f.clone(),
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test execute:: 2>&1 | tail -20`
Expected: PASS — `matches_exact_class_and_projects_object_id`, `respects_limit`.

- [ ] **Step 6: Commit**

```bash
git add src/query/execute.rs src/query/mod.rs
git commit -m "feat(query): add SingleScan executor core (class match, projection, LIMIT)"
```

### Task 9b: SingleScan executor — field decode + WHERE evaluation

**Files:**
- Modify: `src/query/execute.rs` (replace `decode_field` and `where_passes` stubs; extend tests)

- [ ] **Step 1: Write failing tests (in-file)**

Extend the `#[cfg(test)] mod tests` in `src/query/execute.rs` with a fake schema
that resolves a scalar field, and assert decode + WHERE filtering:

```rust
    struct FieldSchema {
        names: std::collections::HashMap<u64, String>,
    }
    impl ClassResolver for FieldSchema {
        fn class_name(&self, class_id: u64) -> Option<&str> {
            self.names.get(&class_id).map(|s| s.as_str())
        }
        fn field(&self, _class_id: u64, name: &str) -> Option<(u32, crate::types::HprofType)> {
            // `count` is an Int at offset 0.
            if name == "count" { Some((0, crate::types::HprofType::Int)) } else { None }
        }
    }

    #[test]
    fn where_filters_on_scalar_field() {
        let q = crate::query::parse::parse("SELECT @objectId FROM C WHERE count > 5").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let sc = FieldSchema {
            names: std::iter::once((10u64, "C".to_string())).collect(),
        };
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        // count = 3 (big-endian i32) -> fails > 5
        ex.visit_instance(1, 10, &[0, 0, 0, 3]);
        // count = 9 -> passes
        ex.visit_instance(2, 10, &[0, 0, 0, 9]);
        let res = ex.finish("q1");
        assert_eq!(res.row_count, 1);
        assert_eq!(res.rows[0][0], crate::query::model::QueryValue::Int(2));
    }

    #[test]
    fn projects_scalar_field_value() {
        let q = crate::query::parse::parse("SELECT count FROM C").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let sc = FieldSchema {
            names: std::iter::once((10u64, "C".to_string())).collect(),
        };
        let mut ex = SingleScanExecutor::new(&q, &plan, &sc);
        ex.visit_instance(1, 10, &[0, 0, 0, 7]);
        let res = ex.finish("q1");
        assert_eq!(res.rows[0][0], crate::query::model::QueryValue::Int(7));
    }
```

- [ ] **Step 2: Verify it fails**

Run: `cargo test execute::tests::where_filters 2>&1 | tail -15`
Expected: FAIL — stub `where_passes` matches all / `decode_field` returns Null.

- [ ] **Step 3: Implement field decode**

Replace the `decode_field` stub in `src/query/execute.rs`:

```rust
    fn decode_field(&self, class_id: u64, name: &str, blob: &[u8]) -> QueryValue {
        use crate::types::HprofType;
        let Some((off, ty)) = self.resolver.field(class_id, name) else {
            return QueryValue::Null;
        };
        let o = off as usize;
        match ty {
            HprofType::Boolean | HprofType::Byte => {
                blob.get(o).map(|&b| {
                    if ty == HprofType::Boolean { QueryValue::Bool(b != 0) }
                    else { QueryValue::Int(b as i64) }
                }).unwrap_or(QueryValue::Null)
            }
            HprofType::Short => read_be(blob, o, 2)
                .map(|v| QueryValue::Int(v as i16 as i64))
                .unwrap_or(QueryValue::Null),
            HprofType::Char => read_be(blob, o, 2)
                .map(|v| QueryValue::Int(v as i64))
                .unwrap_or(QueryValue::Null),
            HprofType::Int => read_be(blob, o, 4)
                .map(|v| QueryValue::Int(v as i32 as i64))
                .unwrap_or(QueryValue::Null),
            HprofType::Long => read_be(blob, o, 8)
                .map(|v| QueryValue::Int(v as i64))
                .unwrap_or(QueryValue::Null),
            HprofType::Float => read_be(blob, o, 4)
                .map(|v| QueryValue::Float(f32::from_bits(v as u32) as f64))
                .unwrap_or(QueryValue::Null),
            HprofType::Double => read_be(blob, o, 8)
                .map(|v| QueryValue::Float(f64::from_bits(v)))
                .unwrap_or(QueryValue::Null),
            // Object (reference) fields: String-field decode is a later slice;
            // project the raw ref as an object index is not available here, so Null.
            HprofType::Object => QueryValue::Null,
        }
    }
```

Add this free function below the `impl` blocks:

```rust
/// Read `n` big-endian bytes at `off` as a u64. None if out of range.
fn read_be(blob: &[u8], off: usize, n: usize) -> Option<u64> {
    if off + n > blob.len() {
        return None;
    }
    let mut v = 0u64;
    for i in 0..n {
        v = (v << 8) | blob[off + i] as u64;
    }
    Some(v)
}
```

- [ ] **Step 4: Implement WHERE evaluation**

Replace the `where_passes` stub. It evaluates the plan's cheapest-first
`where_terms` (all AND-combined at top level; OR/NOT handled recursively):

```rust
    fn where_passes(&self, class_id: u64, blob: &[u8]) -> bool {
        for term in &self.plan.where_terms {
            if !self.eval_pred(&term.pred, class_id, blob) {
                return false;
            }
        }
        true
    }

    fn eval_pred(&self, pred: &crate::query::ast::Predicate, class_id: u64, blob: &[u8]) -> bool {
        use crate::query::ast::Predicate as P;
        match pred {
            P::And(a, b) => self.eval_pred(a, class_id, blob) && self.eval_pred(b, class_id, blob),
            P::Or(a, b) => self.eval_pred(a, class_id, blob) || self.eval_pred(b, class_id, blob),
            P::Not(a) => !self.eval_pred(a, class_id, blob),
            P::InstanceOf(cname) => self
                .resolver
                .class_name(class_id)
                .map(|n| class_name_matches(n, cname))
                .unwrap_or(false),
            P::Compare { lhs, op, rhs } => {
                let lv = self.project_attr(lhs, 0, class_id, blob);
                compare_values(&lv, *op, rhs)
            }
        }
    }
```

Add the comparison helper below `read_be`:

```rust
use crate::query::ast::{CompareOp, Value};

fn compare_values(lv: &QueryValue, op: CompareOp, rhs: &Value) -> bool {
    // Numeric comparisons; string equality; bool equality. Type mismatches
    // compare unequal (never panic).
    let ord = match (lv, rhs) {
        (QueryValue::Int(a), Value::Int(b)) => (*a).partial_cmp(b),
        (QueryValue::Int(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
        (QueryValue::Float(a), Value::Int(b)) => a.partial_cmp(&(*b as f64)),
        (QueryValue::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (QueryValue::Str(a), Value::Str(b)) => Some(a.as_str().cmp(b.as_str())),
        (QueryValue::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (QueryValue::Null, Value::Null) => Some(std::cmp::Ordering::Equal),
        _ => None,
    };
    match ord {
        None => matches!(op, CompareOp::Ne), // mismatch: only `!=` is true
        Some(o) => match op {
            CompareOp::Eq => o.is_eq(),
            CompareOp::Ne => o.is_ne(),
            CompareOp::Lt => o.is_lt(),
            CompareOp::Le => o.is_le(),
            CompareOp::Gt => o.is_gt(),
            CompareOp::Ge => o.is_ge(),
        },
    }
}
```

Note: `project_attr` is called with `src_idx = 0` inside predicate evaluation
because WHERE only references class/field/type data, none of which use the
object index. (@objectId in WHERE is unusual and out of scope this slice.)

- [ ] **Step 5: Run the tests**

Run: `cargo test execute:: 2>&1 | tail -20`
Expected: PASS — `where_filters_on_scalar_field`, `projects_scalar_field_value`,
plus the Task 9a tests still green.

- [ ] **Step 6: Commit**

```bash
git add src/query/execute.rs
git commit -m "feat(query): add field decode and WHERE evaluation to SingleScan executor"
```

---

### Task 9c: Histogram executor — aggregate-only queries from per-class stats

**Files:**
- Create: `src/query/histogram.rs`
- Modify: `src/query/mod.rs` (add `pub mod histogram;`)

A query whose plan `kind == StageKind::HistogramOnly` needs no per-object scan:
its SELECT list is entirely aggregates (`COUNT(*)`, `SUM(@usedHeapSize)`, etc.)
and its FROM/WHERE only constrains by class. The planner (Task 4) already sets
`kind = HistogramOnly` when `needs.instance_scalar`, `needs.instance_string` and
`needs.runtime_type` are all false and every SELECT item is an aggregate. This
executor answers such queries from a small per-class summary that the report
phase already computes (object count + shallow-size total per class), so it runs
in microseconds with zero heap rescans.

- [ ] **Step 1: Write failing tests (in-file)**

Create `src/query/histogram.rs` with the test module first:

```rust
//! Aggregate-only query executor. Answers `SELECT COUNT(*), SUM(@usedHeapSize)
//! FROM <class>` style queries from a per-class summary (count + shallow total),
//! with no per-object heap rescan.

use crate::query::ast::{AggFunc, Attr, Query, SelectItem};
use crate::query::execute::{class_name_matches, column_name};
use crate::query::model::{QueryResult, QueryValue};
use crate::query::plan::QueryPlan;

/// Per-class summary rows the histogram executor reads. One entry per class in
/// the histogram: its normalized name, live instance count, and summed shallow
/// bytes across all instances.
pub struct ClassSummary<'a> {
    pub name: &'a str,
    pub count: u64,
    pub shallow_total: u64,
}

/// Run an aggregate-only query against the class summaries. Caller guarantees
/// `plan.kind == StageKind::HistogramOnly`.
pub fn run_histogram(q: &Query, plan: &QueryPlan, classes: &[ClassSummary]) -> QueryResult {
    let _ = plan; // WHERE on class handled via q.from / class name match
    // Accumulate matched count + shallow across all classes matching FROM.
    let mut count = 0u64;
    let mut shallow = 0u64;
    for c in classes {
        if class_name_matches(c.name, &q.from.class_name) {
            count += c.count;
            shallow += c.shallow_total;
        }
    }
    let mut cols = Vec::new();
    let mut row: Vec<QueryValue> = Vec::new();
    for item in &q.select {
        let (name, val) = eval_agg(item, count, shallow);
        cols.push(crate::query::model::QueryColumn { name });
        row.push(val);
    }
    QueryResult {
        name: String::new(),
        oql: String::new(),
        columns: cols,
        rows: vec![row],
        row_count: 1,
        truncated: false,
        error: None,
    }
}

/// Evaluate one aggregate SELECT item against the count/shallow accumulators.
/// Foundation slice supports COUNT(*) and SUM/AVG over @usedHeapSize only (the
/// two scalars a class summary carries). Other aggregate args resolve to Null —
/// the planner keeps such queries as SingleScan when they need per-object data;
/// anything that still reaches here degrades to Null rather than panicking.
///
/// `arg` is a `Box<SelectItem>` (matching the AST in Task 1): `COUNT(*)` carries
/// `SelectItem::Star`, `SUM(@usedHeapSize)` carries
/// `SelectItem::Attr(Attr::UsedHeapSize)`.
fn eval_agg(item: &SelectItem, count: u64, shallow: u64) -> (String, QueryValue) {
    match item {
        SelectItem::Aggregate { func, arg } => {
            // Column label reuses the executor's `column_name` for consistency
            // with SingleScan output (e.g. "COUNT(*)", "SUM(@usedHeapSize)").
            let label = column_name(item);
            // Is the aggregate argument @usedHeapSize (the one scalar we carry)?
            let arg_is_shallow = matches!(
                arg.as_ref(),
                SelectItem::Attr(Attr::UsedHeapSize)
            );
            let v = match func {
                AggFunc::Count => QueryValue::Int(count as i64),
                AggFunc::Sum if arg_is_shallow => QueryValue::Int(shallow as i64),
                AggFunc::Avg if arg_is_shallow && count > 0 => {
                    QueryValue::Float(shallow as f64 / count as f64)
                }
                // MIN/MAX over a class summary aren't derivable (we keep only the
                // sum), and SUM/AVG over any non-shallow arg is out of scope for
                // the foundation histogram path.
                _ => QueryValue::Null,
            };
            (label, v)
        }
        // Non-aggregate items never reach a HistogramOnly plan.
        _ => (column_name(item), QueryValue::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summaries() -> Vec<ClassSummary<'static>> {
        vec![
            ClassSummary { name: "java.lang.String", count: 100, shallow_total: 2400 },
            ClassSummary { name: "java.util.HashMap", count: 10, shallow_total: 480 },
        ]
    }

    #[test]
    fn count_star_of_one_class() {
        let q = crate::query::parse::parse("SELECT COUNT(*) FROM java.lang.String").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0][0], QueryValue::Int(100));
    }

    #[test]
    fn sum_shallow_of_one_class() {
        let q =
            crate::query::parse::parse("SELECT SUM(@usedHeapSize) FROM java.lang.String").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0][0], QueryValue::Int(2400));
    }

    #[test]
    fn glob_matches_multiple_classes() {
        let q = crate::query::parse::parse("SELECT COUNT(*) FROM java.util.*").unwrap();
        let plan = crate::query::plan::plan_query(&q).unwrap();
        let cs = summaries();
        let res = run_histogram(&q, &plan, &cs);
        assert_eq!(res.rows[0][0], QueryValue::Int(10));
    }
}
```

- [ ] **Step 2: Verify it fails**

Run: `cargo test histogram::tests 2>&1 | tail -20`
Expected: FAIL — `histogram` module not declared / `class_name_matches` and
`column_name` are private in `execute.rs`.

- [ ] **Step 3: Wire the module and export helpers**

In `src/query/mod.rs` add `pub mod histogram;` after `pub mod execute;`.

The two helpers the histogram executor imports — `class_name_matches` and
`column_name` — are already `pub fn` in `execute.rs` (Task 9a Step 4 defines both
as public). No visibility change is needed; the histogram executor reuses
`column_name` for its column labels so headers match the SingleScan path exactly.
If a build error reports either as private, make it `pub`.

- [ ] **Step 4: Run the tests**

Run: `cargo test query:: 2>&1 | tail -20`
Expected: PASS — histogram tests plus all earlier query tests still green.

- [ ] **Step 5: Commit**

```bash
git add src/query/histogram.rs src/query/mod.rs
git commit -m "feat(query): add histogram executor for aggregate-only queries"
```

---

### Task 10: Wire a live ClassResolver over pass2 and run queries during scan

**Files:**
- Create: `src/query/run.rs`
- Modify: `src/query/mod.rs` (add `pub mod run;`)
- Modify: `src/pass2/mod.rs` (build resolver data, drive the executor via the Task 8 visitor)
- Modify: `src/main.rs` (thread queries in, attach results to `Report.queries`)

This is the integration seam. The SingleScan executor implements `ObjectVisitor`
(Task 9a/9b) and must run *during* `scan_heap_2a`, because `class_map`, `strings`
and the field-decode tables are freed inside `Pass2::build` before the report
phase. We build a `LiveResolver` from those tables, construct one
`SingleScanExecutor` per SingleScan query, drive them all through the visitor,
and hand the finished `QueryResult`s back out of `Pass2::build` so `run()` can
attach them to the report. HistogramOnly queries are answered *after* the scan
from the per-class summary the report already computes.

- [ ] **Step 1: Define the live resolver (write it directly — thin adapter, tested via CLI)**

Create `src/query/run.rs`:

```rust
//! Integration seam between the query executors and the live pass2 tables.
//! Builds a `LiveResolver` (class-id -> name, class-id+field -> offset/type)
//! from the pass2 `class_map`/`strings`, and drives every SingleScan query
//! through the `ObjectVisitor` hook during the heap scan.

use std::collections::HashMap;

use crate::pass1::ClassInfo;
use crate::query::execute::{ClassResolver, SingleScanExecutor};
use crate::query::model::QueryResult;
use crate::query::plan::{QueryPlan, StageKind};
use crate::query::ast::Query;
use crate::types::HprofType;

/// A resolver backed by the live pass2 class metadata. Field offsets are
/// computed lazily via `sizing::field_offset` and cached per (class_id, field).
pub struct LiveResolver<'a> {
    class_map: &'a HashMap<u64, ClassInfo>,
    strings: &'a HashMap<u64, String>,
    id_size: usize,
    // (class_addr, field_name) -> resolved offset/type. RefCell so `field`
    // (an &self method on the ClassResolver trait) can memoize.
    cache: std::cell::RefCell<HashMap<(u64, String), Option<(u32, HprofType)>>>,
    // class_addr -> normalized (dotted) class name, memoized.
    name_cache: std::cell::RefCell<HashMap<u64, String>>,
}

impl<'a> LiveResolver<'a> {
    pub fn new(
        class_map: &'a HashMap<u64, ClassInfo>,
        strings: &'a HashMap<u64, String>,
        id_size: usize,
    ) -> Self {
        LiveResolver {
            class_map,
            strings,
            id_size,
            cache: std::cell::RefCell::new(HashMap::new()),
            name_cache: std::cell::RefCell::new(HashMap::new()),
        }
    }

    fn owner_of(&self, class_id: u64, field: &str) -> Option<String> {
        // Walk the super-chain to find the declaring class of `field`.
        let mut cur = class_id;
        loop {
            let ci = self.class_map.get(&cur)?;
            let cname = self.strings.get(&ci.name_id)?;
            for &(fname_id, _t) in &ci.fields {
                if self.strings.get(&fname_id).map(|s| s.as_str()) == Some(field) {
                    return Some(cname.clone());
                }
            }
            if ci.super_id == 0 {
                return None;
            }
            cur = ci.super_id;
        }
    }
}

impl<'a> ClassResolver for LiveResolver<'a> {
    fn class_name(&self, class_id: u64) -> Option<&str> {
        // Memoize normalized name. SAFETY of returned &str: we insert into a
        // RefCell<HashMap<..,String>>; to hand out `&str` tied to &self we leak
        // through the map entry. Use `entry` + return a reference by re-borrow.
        {
            let cache = self.name_cache.borrow();
            if cache.contains_key(&class_id) {
                // fallthrough to the second borrow below
            }
        }
        if !self.name_cache.borrow().contains_key(&class_id) {
            let raw = self.class_map.get(&class_id)
                .and_then(|ci| self.strings.get(&ci.name_id))
                .map(|s| s.replace('/', "."))?;
            self.name_cache.borrow_mut().insert(class_id, raw);
        }
        // We can't return a reference into a RefCell borrow that ends here.
        // Instead store names in a side arena. See Step 2 note.
        None
    }

    fn field(&self, class_id: u64, name: &str) -> Option<(u32, HprofType)> {
        let key = (class_id, name.to_string());
        if let Some(v) = self.cache.borrow().get(&key) {
            return *v;
        }
        let owner = self.owner_of(class_id, name);
        let resolved = owner.and_then(|owner_class| {
            crate::pass2::sizing::field_offset(
                class_id,
                name,
                &owner_class.replace('.', "/"),
                self.class_map,
                self.strings,
                self.id_size,
            )
        });
        self.cache.borrow_mut().insert(key, resolved);
        resolved
    }
}
```

Note on `class_name` returning `&str`: a `RefCell` cache can't hand out a
reference that outlives the borrow. Resolve this by pre-building an owned
`HashMap<u64, String>` of normalized names in the constructor (all class ids are
known up front from `class_map`), storing it as a plain field, and returning
`self.names.get(&class_id).map(String::as_str)`. Replace the `name_cache`
`RefCell` and the stubbed `class_name` accordingly in the next step.

- [ ] **Step 2: Finalize LiveResolver with an owned name map**

Replace the `name_cache` field and `class_name` impl:

```rust
pub struct LiveResolver<'a> {
    class_map: &'a HashMap<u64, ClassInfo>,
    strings: &'a HashMap<u64, String>,
    id_size: usize,
    names: HashMap<u64, String>,
    cache: std::cell::RefCell<HashMap<(u64, String), Option<(u32, HprofType)>>>,
}

impl<'a> LiveResolver<'a> {
    pub fn new(
        class_map: &'a HashMap<u64, ClassInfo>,
        strings: &'a HashMap<u64, String>,
        id_size: usize,
    ) -> Self {
        let mut names = HashMap::with_capacity(class_map.len());
        for (&addr, ci) in class_map {
            if let Some(s) = strings.get(&ci.name_id) {
                names.insert(addr, s.replace('/', "."));
            }
        }
        LiveResolver {
            class_map,
            strings,
            id_size,
            names,
            cache: std::cell::RefCell::new(HashMap::new()),
        }
    }
    // owner_of unchanged
}

impl<'a> ClassResolver for LiveResolver<'a> {
    fn class_name(&self, class_id: u64) -> Option<&str> {
        self.names.get(&class_id).map(|s| s.as_str())
    }
    // field() unchanged
}
```

- [ ] **Step 3: Add the multi-query driver**

Append to `src/query/run.rs` a struct that owns several executors and fans the
visitor callback out to each, plus a helper to split queries by stage:

```rust
/// Drives every SingleScan query through one shared ObjectVisitor pass.
pub struct ScanDriver<'q, R: ClassResolver> {
    execs: Vec<SingleScanExecutor<'q, R>>,
}

impl<'q, R: ClassResolver> ScanDriver<'q, R> {
    pub fn new(execs: Vec<SingleScanExecutor<'q, R>>) -> Self {
        ScanDriver { execs }
    }
    pub fn is_empty(&self) -> bool {
        self.execs.is_empty()
    }
    /// Consume the driver, finishing each executor into a QueryResult.
    pub fn finish(self, names: &[String], oqls: &[String]) -> Vec<QueryResult> {
        self.execs
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                let mut r = e.finish(&names[i]);
                r.oql = oqls.get(i).cloned().unwrap_or_default();
                r
            })
            .collect()
    }
}

impl<'q, R: ClassResolver> crate::query::ObjectVisitor for ScanDriver<'q, R> {
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        for e in &mut self.execs {
            e.visit_instance(src_idx, class_id, blob);
        }
    }
}
```

- [ ] **Step 4: Plumb queries into `Pass2::build`**

The `Query`/`QueryPlan` pairs are parsed+planned in `run()` (Task 11) and passed
into `Pass2::build`. Add a parameter to `Pass2::build` (mod.rs line 58):

```rust
    pub fn build(
        path: &str,
        mut p1: Pass1,
        compress: crate::cvec::Codec,
        opts: &crate::AnalyzeOptions,
        queries: &[(crate::query::ast::Query, crate::query::plan::QueryPlan)],
    ) -> io::Result<(
        Graph,
        InboundBuilder,
        crate::cvec::CompressedU32,
        crate::cvec::CompressedU32,
        Option<crate::cvec::CompressedU32>,
        Vec<crate::query::model::QueryResult>,
    )> {
```

Inside `Pass2::build`, after `class_map`/`strings` are available but before they
are dropped, build the resolver and the SingleScan executors, then pass the
driver into `scan_heap_2a`. SingleScan queries are those with
`plan.kind == StageKind::SingleScan`; HistogramOnly queries are collected
separately and answered after the scan.

Because the executors borrow the `LiveResolver`, which borrows `class_map`/
`strings`, keep all three alive for the duration of the scan. Concretely, near
the existing `scan_heap_2a(...)` call site (mod.rs ~line 345 per the summary):

```rust
    // Build query executors over the live class metadata (borrows class_map +
    // strings; both must outlive the scan). SingleScan queries run in-scan;
    // HistogramOnly queries are answered afterwards from the per-class summary.
    let resolver = crate::query::run::LiveResolver::new(&class_map, &strings, id_size as usize);
    let mut single_names: Vec<String> = Vec::new();
    let mut single_oqls: Vec<String> = Vec::new();
    let mut single_execs = Vec::new();
    for (i, (q, plan)) in queries.iter().enumerate() {
        if matches!(plan.kind, crate::query::plan::StageKind::SingleScan) {
            single_names.push(format!("q{}", i + 1));
            single_oqls.push(String::new()); // filled by run() which has the text
            single_execs.push(crate::query::execute::SingleScanExecutor::new(q, plan, &resolver));
        }
    }
    let mut driver = crate::query::run::ScanDriver::new(single_execs);
    let visitor: Option<&mut dyn crate::query::ObjectVisitor> =
        if driver.is_empty() { None } else { Some(&mut driver) };
```

Pass `visitor` into `scan_heap_2a` (Task 8 added the parameter). After the scan
completes and before `class_map`/`strings` are dropped:

```rust
    let mut query_results = driver.finish(&single_names, &single_oqls);
```

- [ ] **Step 5: Answer HistogramOnly queries after the scan**

Still inside `Pass2::build`, after the graph's per-class shallow totals are
available (or compute a lightweight count/shallow summary here), append the
histogram results. The simplest correct source is a fold over the finished
`graph`: `graph.class_names[graph.class_idx[i]]` for the name and
`graph.shallow[i]` for bytes. Build `ClassSummary` rows once:

```rust
    // Per-class summary for aggregate-only queries.
    if queries.iter().any(|(_, p)| matches!(p.kind, crate::query::plan::StageKind::HistogramOnly)) {
        let class_count = graph.class_names.len();
        let mut counts = vec![0u64; class_count];
        let mut shallow = vec![0u64; class_count];
        for i in 0..graph.class_idx.len() {
            let c = graph.class_idx[i] as usize;
            counts[c] += 1;
            shallow[c] += graph.shallow[i] as u64;
        }
        let summaries: Vec<crate::query::histogram::ClassSummary> = (0..class_count)
            .map(|c| crate::query::histogram::ClassSummary {
                name: &graph.class_names[c],
                count: counts[c],
                shallow_total: shallow[c],
            })
            .collect();
        for (i, (q, plan)) in queries.iter().enumerate() {
            if matches!(plan.kind, crate::query::plan::StageKind::HistogramOnly) {
                let mut r = crate::query::histogram::run_histogram(q, plan, &summaries);
                r.name = format!("q{}", i + 1);
                query_results.push(r);
            }
        }
    }
```

Add `query_results` to the returned tuple: change the final
`Ok((graph, inbound, shallow_c, class_idx_c, alloc_serial_c))` (mod.rs ~line 896)
to `Ok((graph, inbound, shallow_c, class_idx_c, alloc_serial_c, query_results))`.

Note: the field `graph.shallow` may already be compressed at this point. If so,
read it via the same accessor `build_system_overview` uses; otherwise compute
the summary from the still-uncompressed `shallow` vector before compression.
Verify against the code at wiring time and use whichever `shallow`/`class_idx`
source is live where you insert this block.

- [ ] **Step 6: Update the `Pass2::build` caller in main.rs**

At the call site (main.rs ~line 764), destructure the new 6-tuple and pass the
parsed queries:

```rust
    let (graph, inbound, shallow_c, class_idx_c, alloc_serial_c, query_results) =
        Pass2::build(input, p1, compress, &opts, &parsed_queries)?;
```

`parsed_queries: Vec<(Query, QueryPlan)>` is produced in Task 11. For now (this
task, before Task 11 wires the CLI) pass an empty slice `&[]` so the build
compiles, and set `query_results` aside.

- [ ] **Step 7: Build the binary**

Run: `cargo build 2>&1 | tail -20`
Expected: compiles. All query modules + the new `Pass2::build` signature and
caller line up. `query_results` may be unused (prefix `_`) until Task 11.

- [ ] **Step 8: Commit**

```bash
git add src/query/run.rs src/query/mod.rs src/pass2/mod.rs src/main.rs
git commit -m "feat(query): wire live resolver and run queries during heap scan"
```

---

### Task 11: CLI + TOML wiring (`--query`, `--query-file`, `query` subcommand)

**Files:**
- Modify: `src/main.rs` (add analyze flags, parse+plan queries, attach results, add `Cmd::Query`)

- [ ] **Step 1: Add analyze-time query flags to the `Cli` struct**

In `src/main.rs`, add to the top-level `Cli` (near the other analyze options,
around lines 116-174):

```rust
    /// Run an OQL query against the heap and include results in the report.
    /// May be repeated. Example: --query "SELECT * FROM java.lang.String"
    #[arg(long = "query", value_name = "OQL")]
    query: Vec<String>,

    /// Read one OQL query per non-empty line from a file (comments start with #).
    #[arg(long = "query-file", value_name = "PATH")]
    query_file: Option<String>,
```

- [ ] **Step 2: Parse + plan queries early in `run()`**

At the top of `run()` (main.rs line 730), before `Pass2::build`, assemble the
query list from the flags, parse and plan each, and fail fast with a clear error
on the first bad query:

```rust
    // Collect OQL query strings from --query flags and an optional --query-file.
    let mut query_texts: Vec<String> = opts.queries.clone();
    if let Some(ref path) = opts.query_file {
        let body = std::fs::read_to_string(path)?;
        for line in body.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            query_texts.push(t.to_string());
        }
    }
    let mut parsed_queries: Vec<(crate::query::ast::Query, crate::query::plan::QueryPlan)> =
        Vec::with_capacity(query_texts.len());
    for text in &query_texts {
        let q = crate::query::parse::parse(text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("OQL parse error in `{text}`: {}", e.0)))?;
        let plan = crate::query::plan::plan_query(&q)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("OQL plan error in `{text}`: {}", e.0)))?;
        parsed_queries.push((q, plan));
    }
```

Move `queries`/`query_file` into `AnalyzeOptions` (the struct passed as `opts`),
or thread them as separate `run()` params — match the existing convention. If
`AnalyzeOptions` is the carrier, add the two fields to it and populate from the
`Cli` in `main()` where `AnalyzeOptions` is constructed.

- [ ] **Step 3: Attach query results + fill OQL text**

After `Pass2::build` returns `query_results` (Task 10 Step 6), backfill each
result's `oql` text (the executor left it blank) and attach to the report after
`build_model`:

```rust
    // Backfill the original OQL text onto each result (order matches parsed_queries).
    for (r, text) in query_results.iter_mut().zip(query_texts.iter()) {
        if r.oql.is_empty() {
            r.oql = text.clone();
        }
    }
    report.queries = query_results;
```

(`report` is the `mut` binding from `build_model`; it already has
`queries: Vec::new()` from Task 7.)

- [ ] **Step 4: Add a `query` subcommand for ad-hoc one-off runs**

Add to the `Cmd` enum (main.rs line 179):

```rust
    /// Run one or more OQL queries against a heap dump and print results.
    Query {
        /// Path to the .hprof (or .hprof.zip) dump.
        input: String,
        /// OQL query text (may be repeated).
        #[arg(long = "query", value_name = "OQL")]
        query: Vec<String>,
        /// Read queries from a file, one per line.
        #[arg(long = "query-file", value_name = "PATH")]
        query_file: Option<String>,
        /// Start an interactive REPL instead of running fixed queries.
        #[arg(long)]
        repl: bool,
    },
```

- [ ] **Step 5: Dispatch the subcommand in `main()`**

In `main()` (dispatch around lines 369-425), handle `Cmd::Query`. For non-REPL,
route through the same analyze pipeline but render only the query results (call
`run()` with the query flags set and a text/markdown query-only format, or a
dedicated `run_queries()` that prints each `QueryResult` as a table). For
`--repl`, call `crate::query::repl::run_repl(input, ...)` (Task 12). Keep this
thin — reuse `run()` where possible:

```rust
        Some(Cmd::Query { input, query, query_file, repl }) => {
            if repl {
                crate::query::repl::run_repl(&input)?;
            } else {
                let opts = AnalyzeOptions {
                    queries: query,
                    query_file,
                    ..Default::default()
                };
                // Reuse run() with a query-only output mode.
                run(&input, None, OutputFormat::QueriesOnly, false, cvec::Codec::default(), opts)?;
            }
        }
```

Add a `QueriesOnly` variant to `OutputFormat` (or reuse markdown and print only
the query section). If adding the variant, in the render dispatch (main.rs
~lines 974-998) render only `report.queries` via a small table printer.

- [ ] **Step 6: Build**

Run: `cargo build 2>&1 | tail -20`
Expected: compiles.

- [ ] **Step 7: Smoke-test against a fixture**

Run:
```bash
cargo run -- query tests/fixtures/dump_1_mnemonics.hprof.zip --query "SELECT COUNT(*) FROM java.lang.String" 2>&1 | tail -20
```
Expected: prints a one-row table with a COUNT value > 0.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs
git commit -m "feat(query): add --query/--query-file flags and query subcommand"
```

---

### Task 12: Interactive REPL (`!plan`, `!explain`, `!help`, `!quit`)

**Files:**
- Create: `src/query/repl.rs`
- Modify: `src/query/mod.rs` (add `pub mod repl;`)

The REPL loads a dump once, then reads queries from stdin. Bang-commands are
handled locally; anything else is parsed, planned, executed, and printed. Since
each query may need a fresh heap scan (SingleScan), the REPL keeps the live pass2
tables resident is out of scope for the foundation slice — instead it re-runs the
analyze pipeline per query against the already-open dump path. Keep it simple:
parse+plan on every line, print `!plan`/`!explain` without scanning, and run the
scan only for actual queries.

- [ ] **Step 1: Write the REPL (write directly — I/O loop, covered by a CLI test)**

Create `src/query/repl.rs`:

```rust
//! Minimal interactive OQL REPL. Reads one query per line from stdin. Lines
//! beginning with `!` are meta-commands (`!help`, `!plan <oql>`, `!explain
//! <oql>`, `!quit`). Everything else is parsed, planned, executed against the
//! dump at `path`, and printed as a table.

use std::io::{self, BufRead, Write};

use crate::query::model::QueryResult;

pub fn run_repl(path: &str) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    writeln!(stdout, "hprof-analyzer OQL REPL. Type !help for commands, !quit to exit.")?;
    write!(stdout, "oql> ")?;
    stdout.flush()?;
    for line in stdin.lock().lines() {
        let line = line?;
        let t = line.trim();
        if t.is_empty() {
            // reprompt
        } else if let Some(cmd) = t.strip_prefix('!') {
            if handle_meta(cmd, &mut stdout)? {
                break; // !quit
            }
        } else {
            match run_one(path, t) {
                Ok(res) => print_result(&res, &mut stdout)?,
                Err(e) => writeln!(stdout, "error: {e}")?,
            }
        }
        write!(stdout, "oql> ")?;
        stdout.flush()?;
    }
    Ok(())
}

/// Returns Ok(true) if the REPL should exit.
fn handle_meta(cmd: &str, out: &mut impl Write) -> io::Result<bool> {
    let (verb, rest) = match cmd.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (cmd, ""),
    };
    match verb {
        "quit" | "q" | "exit" => return Ok(true),
        "help" | "h" => {
            writeln!(out, "commands:")?;
            writeln!(out, "  !help              show this help")?;
            writeln!(out, "  !plan <oql>        show the query plan (no scan)")?;
            writeln!(out, "  !explain <oql>     alias for !plan")?;
            writeln!(out, "  !quit              exit")?;
            writeln!(out, "  <oql>              run a query and print results")?;
        }
        "plan" | "explain" => match crate::query::parse::parse(rest) {
            Ok(q) => match crate::query::plan::plan_query(&q) {
                Ok(plan) => writeln!(out, "{}", plan.explain())?,
                Err(e) => writeln!(out, "plan error: {}", e.0)?,
            },
            Err(e) => writeln!(out, "parse error: {}", e.0)?,
        },
        other => writeln!(out, "unknown command: !{other} (try !help)")?,
    }
    Ok(false)
}

/// Parse, plan, and execute a single query against the dump at `path`.
fn run_one(path: &str, text: &str) -> io::Result<QueryResult> {
    let q = crate::query::parse::parse(text)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.0))?;
    let plan = crate::query::plan::plan_query(&q)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.0))?;
    // Run pass1 + pass2 with this single query. Reuse the same entry the
    // analyze path uses; a thin helper keeps the REPL decoupled from run().
    let mut results = crate::query::run::run_single_dump(path, &[(q, plan)])?;
    Ok(results.pop().unwrap_or_else(|| QueryResult {
        name: "q1".into(),
        oql: text.into(),
        columns: vec![],
        rows: vec![],
        row_count: 0,
        truncated: false,
        error: Some("no result produced".into()),
    }))
}

/// Print a QueryResult as a simple aligned text table.
fn print_result(res: &QueryResult, out: &mut impl Write) -> io::Result<()> {
    if let Some(err) = &res.error {
        writeln!(out, "error: {err}")?;
        return Ok(());
    }
    let headers: Vec<&str> = res.columns.iter().map(|c| c.name.as_str()).collect();
    writeln!(out, "{}", headers.join(" | "))?;
    for row in &res.rows {
        let cells: Vec<String> = row.iter().map(fmt_value).collect();
        writeln!(out, "{}", cells.join(" | "))?;
    }
    writeln!(out, "({} row{})", res.row_count, if res.row_count == 1 { "" } else { "s" })?;
    if res.truncated {
        writeln!(out, "-- results truncated --")?;
    }
    Ok(())
}

fn fmt_value(v: &crate::query::model::QueryValue) -> String {
    use crate::query::model::QueryValue as V;
    match v {
        V::Null => "null".into(),
        V::Bool(b) => b.to_string(),
        V::Int(i) => i.to_string(),
        V::Float(f) => f.to_string(),
        V::Str(s) => s.clone(),
        V::ObjRef { index, class } => format!("{class}@{index}"),
    }
}
```

- [ ] **Step 2: Add the `run_single_dump` helper to `run.rs`**

The REPL needs a one-shot "run pass1+pass2 for these queries, return results"
function. Add to `src/query/run.rs`:

```rust
/// Run the full pass1+pass2 pipeline against `path` for the given planned
/// queries and return their results. Used by the REPL and the `query`
/// subcommand. Does not build or render the full report.
pub fn run_single_dump(
    path: &str,
    queries: &[(Query, QueryPlan)],
) -> std::io::Result<Vec<QueryResult>> {
    let p1 = crate::pass1::Pass1::parse(path)?;
    let opts = crate::AnalyzeOptions::default();
    let (_g, _inb, _sh, _ci, _al, results) =
        crate::pass2::Pass2::build(path, p1, crate::cvec::Codec::default(), &opts, queries)?;
    Ok(results)
}
```

Verify the exact `Pass1::parse` entry name/signature at wiring time (grep for
`impl Pass1` / `fn parse` in `src/pass1`), and match the `Codec::default()` and
`AnalyzeOptions::default()` conventions used elsewhere.

- [ ] **Step 3: Wire the module**

In `src/query/mod.rs` add `pub mod repl;`.

- [ ] **Step 4: Build**

Run: `cargo build 2>&1 | tail -20`
Expected: compiles.

- [ ] **Step 5: Smoke-test the REPL non-interactively**

Run:
```bash
printf '!plan SELECT * FROM java.lang.String\n!quit\n' | cargo run -- query tests/fixtures/dump_1_mnemonics.hprof.zip --repl 2>&1 | tail -20
```
Expected: prints the plan explanation, then exits cleanly.

- [ ] **Step 6: Commit**

```bash
git add src/query/repl.rs src/query/run.rs src/query/mod.rs
git commit -m "feat(query): add interactive OQL REPL with !plan/!explain/!help/!quit"
```

---

### Task 13: Render a "Custom Queries" section (Markdown + HTML)

**Files:**
- Modify: `src/report/render_md.rs` (append a query section)
- Modify: `src/html.rs` (append a query section)

- [ ] **Step 1: Render Markdown**

In `src/report/render_md.rs`, add a section renderer and call it from
`render_markdown` (line 247) after the existing sections. Only emit when
`!r.queries.is_empty()`:

```rust
fn render_custom_queries(queries: &[crate::query::model::QueryResult], out: &mut String) {
    use std::fmt::Write;
    if queries.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n## Custom Queries\n");
    for q in queries {
        let _ = writeln!(out, "### {}\n", q.name);
        let _ = writeln!(out, "```\n{}\n```\n", q.oql);
        if let Some(err) = &q.error {
            let _ = writeln!(out, "**Error:** {err}\n");
            continue;
        }
        // Header row
        let header: Vec<&str> = q.columns.iter().map(|c| c.name.as_str()).collect();
        let _ = writeln!(out, "| {} |", header.join(" | "));
        let _ = writeln!(out, "|{}", " --- |".repeat(header.len().max(1)));
        for row in &q.rows {
            let cells: Vec<String> = row.iter().map(fmt_query_value).collect();
            let _ = writeln!(out, "| {} |", cells.join(" | "));
        }
        let _ = writeln!(out, "\n_{} row(s){}_\n", q.row_count,
            if q.truncated { ", truncated" } else { "" });
    }
}

fn fmt_query_value(v: &crate::query::model::QueryValue) -> String {
    use crate::query::model::QueryValue as V;
    match v {
        V::Null => "null".into(),
        V::Bool(b) => b.to_string(),
        V::Int(i) => i.to_string(),
        V::Float(f) => format!("{f}"),
        V::Str(s) => s.replace('|', "\\|"),
        V::ObjRef { index, class } => format!("{class}@{index}"),
    }
}
```

Call it in `render_markdown` after the last existing section append:

```rust
    render_custom_queries(&r.queries, &mut out);
```

- [ ] **Step 2: Render HTML**

In `src/html.rs`, in `render_html` (line 52), append an analogous section when
`!r.queries.is_empty()`. Reuse the page's existing table styling. Emit an
`<h2>Custom Queries</h2>`, then per query a `<h3>` name, a `<pre>` OQL block, and
an HTML `<table>` (escape cell text with the existing HTML-escape helper — grep
for `escape` in html.rs and reuse it; do NOT hand-roll escaping):

```rust
    if !r.queries.is_empty() {
        out.push_str("<section><h2>Custom Queries</h2>");
        for q in &r.queries {
            out.push_str(&format!("<h3>{}</h3>", html_escape(&q.name)));
            out.push_str(&format!("<pre>{}</pre>", html_escape(&q.oql)));
            if let Some(err) = &q.error {
                out.push_str(&format!("<p class=\"error\">{}</p>", html_escape(err)));
                continue;
            }
            out.push_str("<table><thead><tr>");
            for c in &q.columns {
                out.push_str(&format!("<th>{}</th>", html_escape(&c.name)));
            }
            out.push_str("</tr></thead><tbody>");
            for row in &q.rows {
                out.push_str("<tr>");
                for cell in row {
                    out.push_str(&format!("<td>{}</td>", html_escape(&fmt_cell(cell))));
                }
                out.push_str("</tr>");
            }
            out.push_str("</tbody></table>");
            if q.truncated {
                out.push_str("<p><em>results truncated</em></p>");
            }
        }
        out.push_str("</section>");
    }
```

Add a `fmt_cell` helper mirroring `fmt_query_value` (plain, no markdown
escaping — HTML escaping is applied by `html_escape`). Match the actual escape
function name/signature found in html.rs.

- [ ] **Step 3: Build**

Run: `cargo build 2>&1 | tail -20`
Expected: compiles.

- [ ] **Step 4: Verify rendered output on a fixture**

Run:
```bash
cargo run -- tests/fixtures/dump_1_mnemonics.hprof.zip --query "SELECT COUNT(*) FROM java.lang.String" --format md 2>&1 | grep -A8 "Custom Queries"
```
Expected: a "## Custom Queries" section with a one-row table.

- [ ] **Step 5: Commit**

```bash
git add src/report/render_md.rs src/html.rs
git commit -m "feat(query): render Custom Queries section in Markdown and HTML"
```

---

### Task 14: Binary-driven integration tests

**Files:**
- Create: `tests/query_cli.rs`

- [ ] **Step 1: Write the integration tests**

Create `tests/query_cli.rs` driving the built binary end-to-end (mirrors the
existing `tests/integration.rs` invocation convention):

```rust
//! End-to-end tests for OQL queries via the `query` subcommand and the
//! `--query` analyze flag. Drives the built binary against a real fixture.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_hprof-analyzer");
const FIXTURE: &str = "tests/fixtures/dump_1_mnemonics.hprof.zip";

fn run(args: &[&str]) -> (String, String, bool) {
    let out = Command::new(BIN)
        .args(args)
        .output()
        .expect("failed to run hprof-analyzer");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn count_star_returns_a_row() {
    let (stdout, stderr, ok) = run(&[
        "query",
        FIXTURE,
        "--query",
        "SELECT COUNT(*) FROM java.lang.String",
    ]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("COUNT"), "stdout: {stdout}");
}

#[test]
fn analyze_flag_embeds_query_section_in_markdown() {
    let (stdout, stderr, ok) = run(&[
        FIXTURE,
        "--query",
        "SELECT COUNT(*) FROM java.lang.String",
        "--format",
        "md",
    ]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("Custom Queries"), "stdout: {stdout}");
}

#[test]
fn bad_query_fails_with_clear_error() {
    let (_stdout, stderr, ok) = run(&["query", FIXTURE, "--query", "SELECT FROM"]);
    assert!(!ok, "expected failure on malformed query");
    assert!(
        stderr.to_lowercase().contains("parse") || stderr.to_lowercase().contains("error"),
        "stderr: {stderr}"
    );
}

#[test]
fn repl_plan_command_prints_plan_without_scan() {
    use std::io::Write;
    let mut child = Command::new(BIN)
        .args(["query", FIXTURE, "--repl"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn repl");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"!plan SELECT * FROM java.lang.String\n!quit\n")
        .unwrap();
    let out = child.wait_with_output().expect("wait repl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        stdout.to_lowercase().contains("scan") || stdout.contains("SingleScan") || stdout.contains("Histogram"),
        "plan output missing stage info: {stdout}"
    );
}
```

Verify the fixture filename exists (`ls tests/fixtures/*.hprof.zip`); the summary
lists `dump_1_mnemonics.hprof.zip` as present. If the class `java.lang.String`
has zero live instances in that fixture, pick a class that does (e.g. inspect
with a quick `--query "SELECT COUNT(*) FROM java.lang.Object"` or use a class the
existing fixtures reference).

- [ ] **Step 2: Run the integration tests**

Run: `cargo test --test query_cli 2>&1 | tail -25`
Expected: all four pass.

- [ ] **Step 3: Run the full test suite (guard against regressions)**

Run: `cargo test 2>&1 | tail -30`
Expected: no regressions — existing fixtures (md/html/json) unchanged because a
query-less run passes `&[]` and the visitor is `None` (byte-identical output).

- [ ] **Step 4: Commit**

```bash
git add tests/query_cli.rs
git commit -m "test(query): add end-to-end CLI + REPL integration tests"
```

---

## Deferred to later slices (explicitly out of scope)

These constructs are **rejected by the planner** (Task 4) with a clear error in
the foundation slice, and are called out here so the next plan can pick them up:

- `DISTINCT`, `AS RETAINED SET`, `OBJECTS`/`INSTANCEOF OBJECTS` result modes
- `UNION`
- `LIKE` / regex predicates
- Reference-hop navigation (`x.field.field`), inbound/outbound edge queries
- `@retainedHeapSize`, dominator-tree predicates
- String-field value decode (`@displayName` for non-String, field==String value)
- `GROUP BY` / `ORDER BY` (our extensions) beyond single-pass `LIMIT`
- MAT oracle differential testing

Each rejection has a test in Task 4 asserting the error message names the
unsupported construct.

## Self-review checklist (run after the plan is complete)

- [ ] Every spec requirement in the foundation slice maps to a task above.
- [ ] No placeholders (`TBD`, "add error handling", bare "write tests").
- [ ] Type/name consistency: `Query`, `QueryPlan`, `QueryNeeds`, `StageKind`,
      `SingleScanExecutor`, `ClassResolver`, `ObjectVisitor`, `QueryResult`,
      `QueryValue`, `ClassSummary`, `LiveResolver`, `ScanDriver` used identically
      across tasks.
- [ ] `Pass2::build` signature/return arity consistent between Task 8, Task 10,
      Task 11, and the REPL helper.
