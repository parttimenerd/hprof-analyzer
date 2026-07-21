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
