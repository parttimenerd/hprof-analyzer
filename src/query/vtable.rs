//! Virtual tables for JOIN sources.
//!
//! Virtual tables are pre-defined read-only result sets available as JOIN RHS
//! without a heap dump scan.  They are identified by double-underscore-delimited
//! names in OQL:
//!
//! ```text
//! SELECT t.name, c.class_name
//!   FROM java.lang.Thread t
//!   INNER JOIN __classes__ AS c ON classof(t) = c.class_name
//! ```
//!
//! Currently provided virtual tables:
//!
//! | Name           | Columns                       | Description                       |
//! |----------------|-------------------------------|-----------------------------------|
//! | `__classes__`  | `class_name`, `instance_count`| All class names seen in the dump  |
//!
//! Additional tables may be added in future slices without breaking the wire format.

// Virtual table helpers are scaffolded here for future use from the JOIN executor.
// They will be wired into `materialise_join_source` in run.rs once VirtualTable JOINs
// are fully supported (requires passing histogram data through the join path).
#![allow(dead_code)]

use crate::query::model::{QueryColumn, QueryResult, QueryValue};

/// Materialise a virtual table by name from the histogram data.
///
/// Returns `Some(QueryResult)` when the name is recognised, `None` when it is
/// unknown (the caller should surface an error).
///
/// `class_histogram` is a slice of `(class_name, instance_count)` pairs in any
/// order (typically from `Pass1`).
pub fn materialise(name: &str, class_histogram: &[(String, u64)]) -> Option<QueryResult> {
    match name {
        "__classes__" => {
            let columns = vec![
                QueryColumn {
                    name: "class_name".into(),
                },
                QueryColumn {
                    name: "instance_count".into(),
                },
            ];
            let rows: Vec<Vec<QueryValue>> = class_histogram
                .iter()
                .map(|(class, count)| {
                    vec![
                        QueryValue::Str(class.clone()),
                        QueryValue::Int((*count).min(i64::MAX as u64) as i64),
                    ]
                })
                .collect();
            let row_count = rows.len() as u64;
            Some(QueryResult {
                name: "__classes__".into(),
                oql: String::new(),
                columns,
                rows,
                row_count,
                truncated: false,
                error: None,
                note: None,
                viz: None,
                elapsed_ms: None,
            })
        }
        _ => None,
    }
}

/// List of all recognised virtual table names (for error messages / help text).
pub const VIRTUAL_TABLE_NAMES: &[&str] = &["__classes__"];
