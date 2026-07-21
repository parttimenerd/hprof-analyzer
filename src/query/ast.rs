//! Parsed OQL query AST. Mirrors the Eclipse MAT OQL surface this analyzer
//! supports; unsupported constructs are rejected in the planner, not here.

/// A parsed query. `union_branches` holds the tail of a homogeneous `UNION`
/// chain (empty for a single query); branches are flat, never nested.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub distinct: bool,
    pub select: Vec<SelectItem>,
    /// `SELECT ... AS RETAINED SET`: expand each result into its full
    /// dominator-retained object set. `false` for a plain projection.
    pub retained_set: bool,
    pub from: ClassSpec,
    pub alias: Option<String>,
    pub where_: Option<Predicate>,
    pub order_by: Option<OrderBy>,
    pub limit: Option<u64>,
    /// `UNION`-separated tail branches, concatenated (UNION ALL semantics).
    /// Each branch is itself a `Query` with an empty `union_branches`.
    pub union_branches: Vec<Query>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir { Asc, Desc }

/// Our extension over MAT OQL: `ORDER BY <attr> [ASC|DESC]`.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderBy {
    pub key: Attr,
    pub dir: SortDir,
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
    /// `@retainedHeapSize` — the object's retained (dominator-subtree) size.
    /// Cross-phase: cannot be answered during the pass2 scan.
    RetainedHeapSize,
    DisplayName,
    Length,
    /// `classof(x)` — the runtime class name.
    ClassOf,
    /// `dominators(alias)` — dominator-tree children of each matched object.
    Dominators(String),
    /// `dominatorof(alias)` — immediate dominator (idom) of each matched object.
    DominatorOf(String),
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
