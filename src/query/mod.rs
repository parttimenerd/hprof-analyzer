//! MAT-style OQL query support: parse, plan, execute against pipeline data,
//! and render into the report. This slice covers histogram-only and
//! single-scan field/class queries; graph/retained/edge primitives are
//! rejected by the planner with a clear message (see the design spec).

pub mod ast;
pub mod parse;
pub mod plan;

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
