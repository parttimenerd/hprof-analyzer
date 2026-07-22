//! MAT-style OQL query support: parse, plan, execute against pipeline data,
//! and render into the report. This slice covers histogram-only and
//! single-scan field/class queries; graph/retained/edge primitives are
//! rejected by the planner with a clear message (see the design spec).

pub mod ast;
pub mod carry;
pub mod execute;
pub mod histogram;
pub mod model;
pub mod optimize;
pub mod parse;
pub mod plan;
pub mod refwalk;
pub mod repl;
pub mod retained_edges;
pub mod run;
pub mod runflags;
pub mod stage_runner;
pub mod stringvals;
pub mod viz;

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

/// Upper bound on the combined row count of a `UNION` result. Each branch is
/// already row-capped individually; this caps the concatenated total so a
/// pathological many-branch union can't blow up memory.
pub const OVERALL_UNION_CAP: usize = 5_000_000;

/// Upper bound on the number of distinct addresses materialized from an
/// `IN (<subquery>)` inner query. Hitting it truncates the membership set
/// (and marks the outer result truncated, since membership is then incomplete).
pub const SUBQUERY_SET_CAP: usize = 5_000_000;

/// Upper bound on the number of frontier objects held while walking a bounded
/// forward subgraph for `path(a, b)`. Caps memory of the path-walk frontier.
pub const PATH_FRONTIER_CAP: usize = 100_000;

/// Default BFS depth cap for `path(a, b)` walks. Bounds memory; override with `--query-path-depth`.
pub const DEFAULT_PATH_DEPTH_CAP: usize = 5;

/// Per-object callback invoked once per INSTANCE_DUMP during the pass2 2a scan,
/// while the raw instance blob and schema tables are still live. Implementors
/// accumulate query matches. Called only when a query is active.
pub trait ObjectVisitor {
    /// `src_idx` is the dense object index; `class_id` the class-object address;
    /// `blob` the raw big-endian instance field bytes.
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]);

    /// Per-array callback for OBJECT_ARRAY_DUMP / PRIMITIVE_ARRAY_DUMP records.
    /// `class_name` is the array's own dotted class name (e.g. `java.lang.Object[]`
    /// or `char[]`) — primitive arrays carry no resolvable class-object address,
    /// so the name is passed directly. `length` is the element count, projected
    /// as `@length`. Defaulted to a no-op so instance-only visitors ignore arrays.
    fn visit_array(&mut self, _src_idx: usize, _class_name: &str, _length: u32) {}
}
