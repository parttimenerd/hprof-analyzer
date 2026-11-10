//! Parsed OQL query AST. Mirrors the Eclipse MAT OQL surface this analyzer
//! supports; unsupported constructs are rejected in the planner, not here.

/// A parsed query. `union_branches` holds the tail of a homogeneous `UNION`
/// chain (empty for a single query); branches are flat, never nested.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Query {
    /// Common Table Expressions from the leading `WITH` clause (empty for
    /// queries without a `WITH`). Only the top-level query carries CTEs; UNION
    /// branches and subqueries always have this field empty.
    pub ctes: Vec<Cte>,
    pub distinct: bool,
    pub select: Vec<SelectItem>,
    /// Per-column AS alias names, parallel to `select`; `None` means no alias.
    pub select_aliases: Vec<Option<String>>,
    /// `SELECT ... AS RETAINED SET`: expand each result into its full
    /// dominator-retained object set. `false` for a plain projection.
    pub retained_set: bool,
    pub from: FromSource,
    pub alias: Option<String>,
    /// JOIN clauses attached to this query's FROM (empty when no JOINs are used).
    pub joins: Vec<JoinClause>,
    pub where_: Option<Predicate>,
    pub order_by: Option<OrderBy>,
    pub limit: Option<u64>,
    /// `OFFSET n` rows to skip after ordering and before applying LIMIT.
    pub offset: Option<u64>,
    /// `UNION`-separated tail branches, concatenated with UNION/UNION ALL semantics
    /// (see `union_all` for the distinction).
    /// Each branch is itself a `Query` with an empty `union_branches`.
    pub union_branches: Vec<Query>,
    /// `true` if this query uses `UNION ALL` (keep duplicates), `false` if plain `UNION` (deduplicate).
    /// Only meaningful when `union_branches` is non-empty; ignored otherwise.
    pub union_all: bool,
    /// Union-wide trailing `LIMIT n` applied to the WHOLE `UNION` result (after
    /// branch concatenation), matching Eclipse MAT. `None` for single queries and
    /// when no trailing union LIMIT is present. Distinct from `limit`, which is
    /// the per-branch/per-query LIMIT.
    pub union_limit: Option<u64>,
    /// GROUP BY expression list (empty = no GROUP BY).
    pub group_by: Vec<Expr>,
    /// HAVING predicate, evaluated post-aggregation (None = no HAVING).
    pub having: Option<Predicate>,
    /// INTERSECT branches (deduplicated intersection with each branch's result set).
    pub intersect_branches: Vec<Query>,
    /// EXCEPT branches (rows in left set not present in any except branch's result set).
    pub except_branches: Vec<Query>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum SortDir {
    Asc,
    Desc,
}

/// Our extension over MAT OQL: `ORDER BY <key> [ASC|DESC] [, <key> [ASC|DESC]]…`
/// Multi-key: left-most key is primary, ties broken by secondary, etc.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct OrderBy {
    pub keys: Vec<(Attr, SortDir)>,
}

/// One projected column.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum SelectItem {
    /// `*` — the object itself (rendered as its display name / ref).
    Star,
    /// An attribute or named field of the FROM alias, e.g. `@usedHeapSize`, `name`.
    Attr(Attr),
    /// An aggregate over all matched instances, e.g. `COUNT(*)`, `SUM(@usedHeapSize)`.
    /// Optional `FILTER (WHERE pred)` restricts which rows contribute.
    Aggregate {
        func: AggFunc,
        arg: Box<SelectItem>,
        /// `COUNT(*) FILTER (WHERE @category = "jdk")` — only rows matching
        /// this predicate contribute to this aggregate. `None` = all rows.
        filter: Option<Predicate>,
    },
    /// `path(a, b)` — a shortest reference path between two operands. Each operand
    /// is either a bound alias (the FROM object) or a class name.
    Path { from: PathOperand, to: PathOperand },
    /// `toString(alias)` — decode a `java.lang.String` instance to its text value.
    /// Only valid when the FROM class is `java.lang.String`. The `String` carries
    /// the FROM alias identifier (e.g. `s` in `toString(s)`).
    ToString(String),
    /// `jsonify(alias [, depth])` — expand the FROM object into a JSON string,
    /// following reference fields recursively. `alias` is the FROM alias
    /// identifier (as in `toString(s)`); `depth` is the optional recursion depth
    /// (levels below the root object), defaulting to
    /// [`crate::query::jsonify::DEFAULT_DEPTH`].
    ///
    /// Unlike `toString`, this reads through `objread` + the cache's
    /// `offsets.bin`, so it is bounded by an object budget and a depth cap and
    /// never re-scans the dump. See `crate::query::jsonify` for the shape.
    Jsonify { alias: String, depth: Option<u32> },
    /// An arithmetic expression column, e.g. `@usedHeapSize * 2`. Only emitted
    /// when the item actually contains an operator; a bare attr stays `Attr`.
    Expr(Box<Expr>),
    /// A window function call: `ROW_NUMBER() OVER (...)`, `LAG(expr, n) OVER (...)`.
    Window {
        func: WindowFunc,
        /// The argument expression for LAG/LEAD (None for ROW_NUMBER/RANK/DENSE_RANK).
        arg: Option<Box<Expr>>,
        over: WindowSpec,
    },
}

/// One operand of a `path(a, b)` select item: a bound alias or a class name.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum PathOperand {
    /// A bound FROM alias, e.g. the `s` in `FROM java.lang.String s`.
    Alias(String),
    /// A class name, e.g. `java.lang.Thread`.
    Class(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AggFunc {
    Count,
    /// COUNT(DISTINCT expr): count of unique non-null values.
    CountDistinct,
    Sum,
    Min,
    Max,
    Avg,
    /// PERCENTILE(<arg>, p): the p-th percentile (nearest-rank) of the numeric
    /// values, p in 1..=100. Collects all values, so it is armed only when used.
    Percentile(u8),
    /// MEDIAN(<arg>): equivalent to PERCENTILE(<arg>, 50).
    Median,
}

/// An attribute reference. `@`-prefixed built-ins plus bare named fields.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Attr {
    ObjectId,
    ObjectAddress,
    UsedHeapSize,
    /// `@retainedHeapSize` — the object's retained (dominator-subtree) size.
    /// Cross-phase: cannot be answered during the pass2 scan.
    RetainedHeapSize,
    DisplayName,
    Length,
    /// `@inbounds` — objects that reference this object (inbound reference edges).
    Inbounds,
    /// `@outbounds` — objects this object references (outbound reference edges).
    Outbounds,
    /// `classof(x)` — the runtime class name.
    ClassOf,
    /// `@category` — classification category of this object's class:
    /// `"jdk"`, `"framework"`, `"other"`, or a user-defined category name.
    /// Requires the classifier; resolved in the late phase.
    Category,
    /// `dominators(alias)` — dominator-tree children of each matched object.
    Dominators(String),
    /// `pathToRoot(alias)` — walk `idom[]` from the given object upward to the
    /// GC root, emitting one row per hop. Same schema as `dominators()` but
    /// walks up instead of collecting children.
    PathToRoot(String),
    /// `dominatorof(alias)` — immediate dominator (idom) of each matched object.
    DominatorOf(String),
    /// `@dominatorClass` — pretty class name of this object's immediate dominator.
    /// Null for GC roots (idom = u32::MAX). Resolved in late phase via `idom[]`.
    DominatorClass,
    /// `@dominatorDepth` — depth of this object in the dominator tree.
    /// Root's direct children = depth 1. Computed by walking `idom[]` upward.
    /// O(depth) per object. Resolved in late phase.
    DominatorDepth,
    /// `@superclass` — pretty class name of this object's class's immediate
    /// superclass. Returns Null for java.lang.Object (no superclass).
    /// Resolved in the late phase via `class_super_idx`.
    Superclass,
    /// `@collectionSize` — logical element count of a known collection class
    /// (e.g. `java.util.ArrayList.size`). Returns `Null` for unrecognised
    /// classes or when the size field is not present.
    CollectionSize,
    /// `@fillRatio` — backing-array fill ratio as a float 0.0–1.0.
    /// `non_null_slots / backing_array_length`. Requires the forward reference
    /// graph (built during full analysis). Returns `Null` for objects that are
    /// not known collections or lack a backing array field.
    FillRatio,
    /// `toString(alias)` — decode a `java.lang.String` instance to its text.
    /// Usable as a WHERE predicate LHS (`WHERE toString(s) LIKE "java.*"`).
    /// Only valid for `java.lang.String` FROM; non-String targets are an error.
    ToString(String),
    /// `toHex(expr)` — format an integer/address as a lowercase `0x…` hex string.
    /// Non-integer argument -> Null (no error).
    ToHex(Box<Expr>),
    /// A bare instance field name, e.g. `count`, `value`.
    Field(String),
    /// An N-hop reference path: `x.parent.name` (after alias-strip) becomes
    /// `RefPath { hops: ["parent"], tail: Field("name") }`. Each hop is a
    /// reference field to follow in the forward-reference graph; the tail is the
    /// scalar/attr projected on the resolved object. Requires ≥ 1 hop (a single
    /// segment after alias-strip stays a plain `Field`).
    RefPath {
        hops: Vec<String>,
        tail: Box<Attr>,
        role: RefRole,
    },
    /// `@valueArray` — a String's backing char/byte `value` array (ObjRef).
    /// Ref-hop attr: resolved in the late phase (P2); scan-time projection is Null.
    ValueArray,
    /// `@referenceArray` — an object-array's element refs, or an instance's
    /// array-typed backing field (e.g. ArrayList.elementData) (ObjRef).
    /// Ref-hop attr: resolved in the late phase (P2); scan-time projection is Null.
    ReferenceArray,
    /// `@GCRoots` — the object's GC-root entries (empty if not a root).
    /// Analyze-pipeline only; Null in the query-only path.
    GcRoots,
    /// `@GCRootInfo` / `@info` — root descriptor detail (root type/tag) for a
    /// root object; empty/Null for a non-root. Analyze-pipeline only.
    GcRootInfo,
    /// `base[index]` — single 0-based element of an array-valued attr.
    /// Out-of-bounds or non-array base → Null (not an error).
    /// Resolved in P2 (late window).
    ArrayIndex {
        base: Box<Attr>,
        index: Box<Expr>,
    },
    /// `base[start:end]` — slice of an array-valued attr.
    /// start/end are optional (None = beginning/end).
    /// Resolved in P2 (late window). Result: JSON-encoded string.
    ArraySlice {
        base: Box<Attr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
    },
}

/// An arithmetic expression over attribute/field/literal operands. Leaves are
/// the existing `Attr` and `Value` nodes; `Binary`/`Unary` compose them. Used in
/// SELECT columns and WHERE comparison operands. A single-leaf expression is
/// folded back to `SelectItem::Attr` / a plain compare by the parser, so
/// no-arithmetic queries never carry an `Expr`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Expr {
    Attr(Attr),
    Lit(Value),
    Binary {
        op: ArithOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        arg: Box<Expr>,
    },
    /// A method/property invocation on a resolved receiver: `receiver.name(args)`.
    /// Zero-arg for property forms. Dispatch is a FIXED table (MAT-API aliases +
    /// emulated JDK methods) keyed on name + receiver class — NOT reflection.
    /// Unknown methods are rejected at plan time (D5).
    Method {
        receiver: Box<Expr>,
        name: String,
        args: Vec<Expr>,
    },
    /// An aggregate function call used in HAVING position, e.g. `COUNT(*)`,
    /// `SUM(@usedHeapSize)`. Valid only inside a HAVING clause (the planner
    /// rejects aggregate expressions in WHERE or GROUP BY keys).
    Aggregate {
        func: AggFunc,
        arg: Box<SelectItem>,
    },
    /// `CASE WHEN <pred> THEN <expr> [WHEN <pred> THEN <expr>]* [ELSE <expr>] END`
    /// Branches evaluated left-to-right; first match wins.
    /// If no branch matches and `else_` is None, result is Null.
    Case {
        branches: Vec<(Predicate, Expr)>,
        else_: Option<Box<Expr>>,
    },
    /// `COALESCE(expr, expr, ...)` — first non-Null value, or Null if all Null.
    Coalesce(Vec<Expr>),
    /// `NULLIF(a, b)` — Null if a == b, else a.
    NullIf {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `CAST(expr AS type)` — explicit type conversion.
    Cast {
        expr: Box<Expr>,
        target: CastTarget,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    /// `%` — modulo (remainder). `a % 0` → Null; `i64::MIN % -1` uses
    /// `wrapping_rem` (Java `long` semantics).
    Mod,
}

/// Target type for `CAST(expr AS type)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CastTarget {
    Int,
    Float,
    Str,
    Bool,
}

/// A window function type.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum WindowFunc {
    RowNumber,
    Rank,
    DenseRank,
    /// LAG(expr, offset, default) — value from `offset` rows before.
    Lag {
        offset: u32,
        default_val: Option<Box<Expr>>,
    },
    /// LEAD(expr, offset, default) — value from `offset` rows after.
    Lead {
        offset: u32,
        default_val: Option<Box<Expr>>,
    },
}

/// Window specification: OVER ([PARTITION BY ...] [ORDER BY ...]).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WindowSpec {
    pub partition_by: Vec<Expr>,
    pub order_by: Vec<(Expr, SortDir)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum UnaryOp {
    Neg,
    Pos,
}

impl Expr {
    /// The single `Attr` leaf, if this expression is exactly `Expr::Attr`.
    pub fn as_attr(&self) -> Option<&Attr> {
        match self {
            Expr::Attr(a) => Some(a),
            _ => None,
        }
    }
    /// The single `Value` leaf, if this expression is exactly `Expr::Lit`.
    pub fn as_lit(&self) -> Option<&Value> {
        match self {
            Expr::Lit(v) => Some(v),
            _ => None,
        }
    }
}

/// When a `RefPath` must be resolved. Predicate-critical paths (used in WHERE)
/// resolve before row filtering; projection-only paths (used only in SELECT)
/// resolve after filtering (cheaper — fewer rows). Assigned during planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum RefRole {
    PredicateCritical,
    ProjectionOnly,
}

/// The FROM clause target.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ClassSpec {
    /// true for `FROM INSTANCEOF C` (subclasses included), false for `FROM C`.
    pub instanceof: bool,
    /// The class name (or regex source) as written, e.g. `java.lang.String`,
    /// `com.acme.*`, or (when `is_regex`) a Java-style regex like `java\.lang\..*`.
    pub class_name: String,
    /// true when the FROM target was a DOUBLE-QUOTED string (MAT regex form):
    /// `class_name` is then a Java-style regex matched full/anchored against the
    /// object's dotted class name. false for a bare identifier/glob target, which
    /// uses the exact/glob matcher. Quoted regex is invalid with INSTANCEOF.
    pub is_regex: bool,
}

/// A FROM source: either a class pattern or a nested (non-correlated) subquery
/// whose result set the outer query scans. `Subquery` boxes the inner `Query`
/// to keep `Query` a fixed size.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum FromSource {
    Class(ClassSpec),
    Subquery(Box<Query>),
    /// A single heap object identified by address (decimal or hex literal, e.g.
    /// `FROM OBJECTS 0x1295e2f8`). Resolved to one dense index via the
    /// address->index map; a missing address yields zero rows (MAT parity).
    Object(u64),
    /// `FROM ENTRIES <ClassName>` — iterate over the logical *elements* of a
    /// known collection class (e.g. `java.util.ArrayList`). Each row is one
    /// element reference, not the collection object itself. `class` is the
    /// dotted class name as written (e.g. `java.util.ArrayList`).
    Entries {
        class: String,
    },
    /// Inline value table: `FROM (VALUES (1,'a'), (2,'b')) AS t(id, name)`
    Values {
        rows: Vec<Vec<Value>>,
        alias: String,
        columns: Vec<String>,
    },
}

impl FromSource {
    /// The FROM class pattern as written, or `""` for a subquery source (which
    /// has no class name of its own — its shape comes from the inner query).
    pub fn class_name(&self) -> &str {
        match self {
            FromSource::Class(c) => &c.class_name,
            FromSource::Subquery(_) => "",
            FromSource::Object(_) => "",
            FromSource::Entries { class } => class,
            FromSource::Values { .. } => "",
        }
    }
    /// The class spec for a class FROM, or `None` for a subquery source.
    pub fn class_spec(&self) -> Option<&ClassSpec> {
        match self {
            FromSource::Class(c) => Some(c),
            FromSource::Subquery(_) => None,
            FromSource::Object(_) => None,
            FromSource::Entries { .. } => None,
            FromSource::Values { .. } => None,
        }
    }
    /// Whether this is `FROM INSTANCEOF C` (subclasses included). `false` for a
    /// subquery source.
    pub fn instanceof(&self) -> bool {
        matches!(self, FromSource::Class(c) if c.instanceof)
    }
    /// The inner query for a subquery source, else `None`.
    pub fn as_subquery(&self) -> Option<&Query> {
        match self {
            FromSource::Subquery(q) => Some(q),
            FromSource::Class(_) => None,
            FromSource::Object(_) => None,
            FromSource::Entries { .. } => None,
            FromSource::Values { .. } => None,
        }
    }
}

/// A WHERE predicate tree.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Predicate {
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
    Not(Box<Predicate>),
    /// `true` or `false` literal — useful in `WHERE true` / `WHERE false`.
    Bool(bool),
    Compare {
        lhs: Expr,
        op: CompareOp,
        rhs: Expr,
    },
    /// `x INSTANCEOF C`
    InstanceOf(String),
    /// `<lhs> IN ( <inner> )` — keep rows whose `lhs` attribute is a member of
    /// the (non-correlated) inner query's result set. `inner` has an empty
    /// `union_branches` (UNION is not allowed inside a subquery).
    InSubquery {
        lhs: Attr,
        inner: Box<Query>,
    },
    /// `EXISTS (<subquery>)` / `NOT EXISTS (<subquery>)`.
    /// Non-correlated: evaluated once before the outer scan.
    Exists {
        inner: Box<Query>,
        negated: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `LIKE` — RHS is a Java-style regex matched with FULL/ANCHORED semantics
    /// (like `java.util.regex.Pattern.matches`, i.e. the whole string must
    /// match), NOT a SQL glob. Meaningful only for string LHS/RHS.
    Like,
    /// `NOT LIKE` — negation of [`CompareOp::Like`].
    NotLike,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
}

// ──────────────────────────────────────────────────────────────────────────────
// Feature #11: Common Table Expressions (WITH / CTE)
// ──────────────────────────────────────────────────────────────────────────────

/// A single Common Table Expression defined in a `WITH` clause:
/// `WITH <name> AS (<query>)`.
///
/// CTEs are stored on the top-level [`Query`] in the `ctes` field. The planner
/// resolves `FROM <name>` references to `FromSource::CteRef` when the name
/// matches a CTE bound in the same statement. Execution materialises each CTE
/// result once (in declaration order) before running the main query body.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Cte {
    /// The name bound by `WITH <name> AS (...)`. Must be unique within the
    /// same `WITH` clause (enforced at plan time).
    pub name: String,
    /// The query body that produces the CTE's rows.
    pub query: Query,
}

// ──────────────────────────────────────────────────────────────────────────────
// JOIN support
// ──────────────────────────────────────────────────────────────────────────────

/// Kind of a SQL JOIN clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum JoinKind {
    /// `INNER JOIN` — only rows with a matching RHS row are emitted.
    Inner,
    /// `LEFT JOIN` / `LEFT OUTER JOIN` — all LHS rows are emitted; unmatched
    /// rows get `Null` for all RHS columns.
    Left,
}

/// The right-hand side source of a JOIN clause.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum JoinSource {
    /// An inline VALUES table: `JOIN (VALUES (…), …) AS t(a, b) ON …`
    Values {
        rows: Vec<Vec<Value>>,
        alias: String,
        columns: Vec<String>,
    },
    /// A named virtual table provided by the engine: `JOIN __classes__ AS c ON …`
    /// Virtual table names are double-underscore-delimited identifiers.
    VirtualTable { name: String, alias: String },
    /// A subquery: `JOIN (SELECT … FROM …) AS t ON …`
    Subquery { query: Box<Query>, alias: String },
}

impl JoinSource {
    /// The alias bound for the RHS table (used to resolve column refs like `t.name`).
    #[allow(dead_code)]
    pub fn alias(&self) -> &str {
        match self {
            JoinSource::Values { alias, .. } => alias,
            JoinSource::VirtualTable { alias, .. } => alias,
            JoinSource::Subquery { alias, .. } => alias,
        }
    }
}

/// The ON condition of a JOIN: `ON lhs_alias.col = rhs_alias.col`.
/// We support only equality predicates for now (hash-join implementation).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JoinOn {
    /// LHS column expression (typically `alias.field` or an `@attr`).
    pub lhs: Expr,
    /// RHS column expression (typically `rhs_alias.field`).
    pub rhs: Expr,
}

/// A single JOIN clause attached to a FROM.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JoinClause {
    pub kind: JoinKind,
    pub source: JoinSource,
    pub on: JoinOn,
}
