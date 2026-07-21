//! MAT-style OQL query support: parse, plan, execute against pipeline data,
//! and render into the report. This slice covers histogram-only and
//! single-scan field/class queries; graph/retained/edge primitives are
//! rejected by the planner with a clear message (see the design spec).

pub mod ast;
pub mod parse;
pub mod plan;
pub mod model;
pub mod execute;
pub mod histogram;

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

/// Per-object callback invoked once per INSTANCE_DUMP during the pass2 2a scan,
/// while the raw instance blob and schema tables are still live. Implementors
/// accumulate query matches. Called only when a query is active.
pub trait ObjectVisitor {
    /// `src_idx` is the dense object index; `class_id` the class-object address;
    /// `blob` the raw big-endian instance field bytes.
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]);
}
