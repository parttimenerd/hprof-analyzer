//! Virtual tables: named read-only tables materialized from pipeline data.
//!
//! They are identified by double-underscore-delimited names in OQL and work BOTH
//! as a FROM target and as a JOIN right-hand side:
//!
//! ```text
//! SELECT class_name, instance_count FROM __classes__ LIMIT 10
//! SELECT t.name, c.class_name
//!   FROM java.lang.Thread t
//!   INNER JOIN __classes__ AS c ON classof(t) = c.class_name
//! ```
//!
//! Provided virtual tables (names stored WITHOUT the `__` delimiters):
//!
//! | Name        | Columns                                                                    | Rows                              |
//! |-------------|----------------------------------------------------------------------------|-----------------------------------|
//! | `classes`   | `class_name`, `instance_count`, `shallow_total`, `retained_total`           | one per class in the histogram    |
//! | `gc_roots`  | `dense_idx`, `root_type`, `root_type_name`                                  | one per GC root                   |
//! | `threads`   | `serial`, `obj_idx`, `name`, `is_daemon`, `priority`                        | one per thread record (stack)     |
//!
//! Rows are built from post-scan pipeline data (`VTableData`, projected off the
//! `Graph`) — no per-object heap rescan. `retained_total` is Null unless the run
//! computed retained sizes (the FROM path materializes before the late stage;
//! the JOIN path can see them when the caller has them).

use crate::pass2::ThreadStack;
use crate::query::ast::Query;
use crate::query::model::{QueryColumn, QueryResult, QueryValue};
use crate::query::plan::QueryPlan;
use std::collections::HashMap;

/// All supported virtual table names (WITHOUT the `__` delimiters; used for
/// error messages and help text).
pub const VIRTUAL_TABLE_NAMES: &[&str] = &["classes", "gc_roots", "threads"];

/// The pipeline slices a virtual table's rows are materialized from. Borrowed
/// off the assembled post-scan `Graph` (see `run.rs::materialise_join_source`
/// and pass2's virtual-table push). `retained` may be empty — `retained_total`
/// is then Null.
pub struct VTableData<'a> {
    /// Dense object count.
    pub n: usize,
    /// Class histogram names (one per class row index).
    pub class_names: &'a [String],
    /// Dense object index → class row index.
    pub class_idx: &'a [u32],
    /// Dense object shallow sizes (bytes).
    pub shallow: &'a [u32],
    /// Dense object retained sizes (empty when not computed).
    pub retained: &'a [u64],
    /// GC-root dense indices.
    pub gc_root_indices: &'a [u32],
    /// GC-root tag per root (parallel to `gc_root_indices`).
    pub gc_root_types: &'a [u8],
    /// One record per thread with a stack trace.
    pub thread_stacks: &'a [ThreadStack],
    /// Per-thread decoded properties, keyed by thread serial.
    pub thread_props: &'a HashMap<u32, crate::pass2::ThreadProps>,
}

/// Materialize the named virtual table from pipeline data.
///
/// Returns `(column_names, rows)` when the name is recognised, `None` when it
/// is unknown (the caller should surface an explicit "not supported" error).
pub fn materialise(name: &str, data: &VTableData) -> Option<(Vec<String>, Vec<Vec<QueryValue>>)> {
    match name {
        "classes" => {
            let cols = vec![
                "class_name".to_string(),
                "instance_count".to_string(),
                "shallow_total".to_string(),
                "retained_total".to_string(),
            ];
            let n_classes = data.class_names.len();
            let mut inst = vec![0u64; n_classes];
            let mut shallow = vec![0u64; n_classes];
            for i in 0..data.n {
                let ci = data.class_idx.get(i).copied().unwrap_or(u32::MAX) as usize;
                if ci < n_classes {
                    inst[ci] += 1;
                    shallow[ci] += data.shallow.get(i).copied().unwrap_or(0) as u64;
                }
            }
            let has_retained = !data.retained.is_empty();
            let mut retained = vec![0u64; n_classes];
            if has_retained {
                for i in 0..data.n {
                    let ci = data.class_idx.get(i).copied().unwrap_or(u32::MAX) as usize;
                    if ci < n_classes {
                        retained[ci] += data.retained.get(i).copied().unwrap_or(0);
                    }
                }
            }
            let rows = (0..n_classes)
                .map(|ci| {
                    vec![
                        QueryValue::Str(data.class_names[ci].clone()),
                        QueryValue::Int(inst[ci].min(i64::MAX as u64) as i64),
                        QueryValue::Int(shallow[ci].min(i64::MAX as u64) as i64),
                        if has_retained {
                            QueryValue::Int(retained[ci].min(i64::MAX as u64) as i64)
                        } else {
                            QueryValue::Null
                        },
                    ]
                })
                .collect();
            Some((cols, rows))
        }
        "gc_roots" => {
            let cols = vec![
                "dense_idx".to_string(),
                "root_type".to_string(),
                "root_type_name".to_string(),
            ];
            let rows = data
                .gc_root_indices
                .iter()
                .zip(data.gc_root_types.iter())
                .map(|(&idx, &t)| {
                    vec![
                        QueryValue::Int(idx as i64),
                        QueryValue::Int(t as i64),
                        QueryValue::Str(gc_root_type_name(t)),
                    ]
                })
                .collect();
            Some((cols, rows))
        }
        "threads" => {
            let cols = vec![
                "serial".to_string(),
                "obj_idx".to_string(),
                "name".to_string(),
                "is_daemon".to_string(),
                "priority".to_string(),
            ];
            let rows = data
                .thread_stacks
                .iter()
                .map(|ts| {
                    let props = data.thread_props.get(&ts.thread_serial);
                    vec![
                        QueryValue::Int(ts.thread_serial as i64),
                        QueryValue::Int(ts.thread_obj_idx as i64),
                        QueryValue::Str(props.map(|p| p.name.clone()).unwrap_or_default()),
                        QueryValue::Bool(props.map(|p| p.is_daemon).unwrap_or(false)),
                        QueryValue::Int(props.map(|p| p.priority as i64).unwrap_or(0)),
                    ]
                })
                .collect();
            Some((cols, rows))
        }
        _ => None,
    }
}

fn gc_root_type_name(tag: u8) -> String {
    match tag {
        0x01 => "JNI Global".into(),
        0x02 => "JNI Local".into(),
        0x03 => "Java Frame".into(),
        0x05 => "Sticky Class".into(),
        0x08 => "Thread Object".into(),
        _ => format!("root_type_0x{tag:02x}"),
    }
}

/// Column count of each supported virtual table (for `SELECT *` arity).
pub fn arity(name: &str) -> Option<usize> {
    match name {
        "classes" => Some(4),
        "gc_roots" => Some(3),
        "threads" => Some(5),
        _ => None,
    }
}

/// Run a `FROM <virtual table>` query end-to-end: materialize the table rows,
/// then apply WHERE / DISTINCT / ORDER BY / OFFSET / LIMIT against the
/// materialized rows (no heap scan). Caller guarantees
/// `plan.kind == StageKind::VirtualTable` and that plan-time validation
/// (`validate_virtual_table_query` in plan.rs) already rejected unsupported
/// clauses; this function degrades unsupported SELECT items to Null rather
/// than panicking.
pub fn run_virtual_table_query(
    q: &Query,
    plan: &QueryPlan,
    data: Option<&VTableData>,
) -> QueryResult {
    use crate::query::execute::{
        apply_window_functions, canonical_distinct_key_pub as canonical_key, query_columns,
        resolve_order_by_keys, sort_rows_by_columns,
    };

    let Some(name) = q.from.as_virtual_table() else {
        return error_result("run_virtual_table_query called on non-virtual-table query");
    };
    let Some(data) = data else {
        return error_result(
            "virtual table query requires a heap-scan run context \
             (pass the assembled pipeline data)",
        );
    };
    let Some((cols, rows)) = materialise(name, data) else {
        return error_result(&format!(
            "virtual table `__{name}__` is not supported; supported virtual tables: {}",
            VIRTUAL_TABLE_NAMES
                .iter()
                .map(|n| format!("__{n}__"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };

    // WHERE filtering on the materialized rows (column-based predicates).
    let filtered: Vec<&Vec<QueryValue>> = if let Some(pred) = &q.where_ {
        rows.iter()
            .filter(|row| crate::query::run::eval_pred_on_row(pred, &cols, row) == Some(true))
            .collect()
    } else {
        rows.iter().collect()
    };

    let is_star = q.select.len() == 1 && matches!(q.select[0], crate::query::ast::SelectItem::Star);
    let out_cols: Vec<QueryColumn> = if is_star {
        cols.iter()
            .map(|c| QueryColumn { name: c.clone() })
            .collect()
    } else {
        query_columns(q)
    };

    // Project each row to the SELECT list (mirrors execute_values_query).
    let project_row = |row: &[QueryValue]| -> Vec<QueryValue> {
        if is_star {
            return row.to_vec();
        }
        q.select
            .iter()
            .map(|item| match item {
                crate::query::ast::SelectItem::Attr(crate::query::ast::Attr::Field(f)) => {
                    let bare = f.rsplit_once('.').map(|(_, col)| col).unwrap_or(f.as_str());
                    let pos = cols.iter().position(|c| c == f).or_else(|| {
                        if bare != f {
                            cols.iter().position(|c| c == bare)
                        } else {
                            None
                        }
                    });
                    pos.and_then(|p| row.get(p))
                        .cloned()
                        .unwrap_or(QueryValue::Null)
                }
                // A single-hop `t.col` RefPath is table qualification.
                crate::query::ast::SelectItem::Attr(crate::query::ast::Attr::RefPath {
                    hops,
                    tail,
                    role: crate::query::ast::RefRole::ProjectionOnly,
                }) if hops.len() == 1 => {
                    if let crate::query::ast::Attr::Field(col) = tail.as_ref() {
                        cols.iter()
                            .position(|c| c == col)
                            .and_then(|p| row.get(p))
                            .cloned()
                            .unwrap_or(QueryValue::Null)
                    } else {
                        QueryValue::Null
                    }
                }
                crate::query::ast::SelectItem::Expr(expr) => {
                    crate::query::run::eval_expr_on_row(expr, &cols, row)
                }
                _ => QueryValue::Null,
            })
            .collect()
    };

    let mut out_rows: Vec<Vec<QueryValue>> = filtered.iter().map(|r| project_row(r)).collect();

    // E-05: SELECT DISTINCT dedup before OFFSET/LIMIT.
    if q.distinct {
        let mut seen = std::collections::HashSet::with_capacity(out_rows.len());
        out_rows.retain(|row| {
            let key: String = row
                .iter()
                .map(canonical_key)
                .collect::<Vec<_>>()
                .join("\x00");
            seen.insert(key)
        });
    }

    // ORDER BY over the projected rows.
    if let Some(order_by) = &q.order_by {
        let keys = resolve_order_by_keys(q, &out_cols, order_by);
        sort_rows_by_columns(&mut out_rows, &keys);
    }
    apply_window_functions(&mut out_rows, &out_cols, q, 0, &[]);

    // OFFSET first, then LIMIT — with OQL-4 truncation semantics: a LIMIT that
    // actually caps rows marks truncated; LIMIT 0 is an explicit empty request
    // and never marks truncated.
    if let Some(off) = q.offset {
        let n = (off as usize).min(out_rows.len());
        if n > 0 {
            out_rows.drain(..n);
        }
    }
    let truncated = if let Some(lim) = q.limit {
        let lim = lim as usize;
        if out_rows.len() > lim {
            out_rows.truncate(lim);
            lim > 0
        } else {
            false
        }
    } else {
        false
    };
    let _ = plan;

    let row_count = out_rows.len() as u64;
    QueryResult {
        name: String::new(),
        oql: String::new(),
        columns: out_cols,
        rows: out_rows,
        row_count,
        truncated,
        error: None,
        note: None,
        viz: None,
        elapsed_ms: None,
    }
}

fn error_result(msg: &str) -> QueryResult {
    QueryResult {
        name: String::new(),
        oql: String::new(),
        columns: Vec::new(),
        rows: Vec::new(),
        row_count: 0,
        truncated: false,
        error: Some(msg.to_string()),
        note: None,
        viz: None,
        elapsed_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass2::ThreadProps;
    use crate::query::parse::parse;
    use crate::query::plan::plan_query;

    fn pq(q: &Query) -> QueryPlan {
        plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap()
    }

    fn data() -> VTableData<'static> {
        // Leak the fixture slices to get 'static borrows — tiny test-only data.
        let class_names: &'static [String] = Box::leak(
            vec![
                "java.lang.String".to_string(),
                "java.lang.Thread".to_string(),
            ]
            .into_boxed_slice(),
        );
        let class_idx: &'static [u32] = Box::leak(vec![0u32, 0, 1].into_boxed_slice());
        let shallow: &'static [u32] = Box::leak(vec![24u32, 32, 40].into_boxed_slice());
        let gc_root_indices: &'static [u32] = Box::leak(vec![0u32, 2].into_boxed_slice());
        let gc_root_types: &'static [u8] = Box::leak(vec![0x08u8, 0x01].into_boxed_slice());
        let thread_stacks: &'static [ThreadStack] = Box::leak(
            vec![ThreadStack {
                thread_serial: 1,
                thread_obj_idx: 2,
                frames: vec![],
            }]
            .into_boxed_slice(),
        );
        let thread_props: &'static HashMap<u32, ThreadProps> = {
            let mut m: HashMap<u32, ThreadProps> = HashMap::new();
            m.insert(
                1u32,
                ThreadProps {
                    name: "main".into(),
                    is_daemon: false,
                    priority: 5,
                    ..Default::default()
                },
            );
            Box::leak(m.into())
        };
        VTableData {
            n: 3,
            class_names,
            class_idx,
            shallow,
            retained: &[],
            gc_root_indices,
            gc_root_types,
            thread_stacks,
            thread_props,
        }
    }

    #[test]
    fn classes_table_rows() {
        let (cols, rows) = materialise("classes", &data()).unwrap();
        assert_eq!(
            cols,
            vec![
                "class_name",
                "instance_count",
                "shallow_total",
                "retained_total"
            ]
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], QueryValue::Str("java.lang.String".into()));
        assert_eq!(rows[0][1], QueryValue::Int(2));
        assert_eq!(rows[0][2], QueryValue::Int(56));
        // No retained data in this fixture → Null retained_total.
        assert_eq!(rows[0][3], QueryValue::Null);
    }

    #[test]
    fn gc_roots_table_rows() {
        let (_cols, rows) = materialise("gc_roots", &data()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], QueryValue::Int(0));
        assert_eq!(rows[0][2], QueryValue::Str("Thread Object".into()));
        assert_eq!(rows[1][2], QueryValue::Str("JNI Global".into()));
    }

    #[test]
    fn threads_table_rows() {
        let (_cols, rows) = materialise("threads", &data()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], QueryValue::Int(1));
        assert_eq!(rows[0][2], QueryValue::Str("main".into()));
        assert_eq!(rows[0][3], QueryValue::Bool(false));
        assert_eq!(rows[0][4], QueryValue::Int(5));
    }

    #[test]
    fn unknown_table_is_none() {
        assert!(materialise("bogus", &data()).is_none());
    }

    #[test]
    fn from_classes_query_runs() {
        let q = parse("SELECT class_name, instance_count FROM __classes__ LIMIT 1").unwrap();
        let plan = pq(&q);
        assert_eq!(plan.kind, crate::query::plan::StageKind::VirtualTable);
        let r = run_virtual_table_query(&q, &plan, Some(&data()));
        assert!(r.error.is_none(), "error: {:?}", r.error);
        assert_eq!(r.row_count, 1);
        assert_eq!(r.rows[0][0], QueryValue::Str("java.lang.String".into()));
        // LIMIT 1 over 2 rows → truncated.
        assert!(r.truncated);
    }

    #[test]
    fn from_classes_limit_zero_is_explicit_empty() {
        let q = parse("SELECT * FROM __classes__ LIMIT 0").unwrap();
        let plan = pq(&q);
        let r = run_virtual_table_query(&q, &plan, Some(&data()));
        assert!(r.error.is_none());
        assert_eq!(r.row_count, 0);
        assert!(!r.truncated, "LIMIT 0 must not mark truncated (OQL-4)");
    }

    #[test]
    fn where_and_order_by_work() {
        let q = parse(
            "SELECT class_name FROM __classes__ WHERE instance_count > 1 ORDER BY class_name DESC",
        )
        .unwrap();
        let plan = pq(&q);
        let r = run_virtual_table_query(&q, &plan, Some(&data()));
        assert!(r.error.is_none(), "error: {:?}", r.error);
        assert_eq!(r.row_count, 1);
        assert_eq!(r.rows[0][0], QueryValue::Str("java.lang.String".into()));
    }

    #[test]
    fn unsupported_table_is_rejected_at_plan_time() {
        // An unknown virtual table must fail LOUDLY at plan time (not silently
        // match 0 heap classes, the pre-B6a behavior).
        let q = parse("SELECT * FROM __bogus__").unwrap();
        let err = plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP)
            .expect_err("__bogus__ must be rejected at plan time");
        assert!(
            err.0.contains("__bogus__") && err.0.contains("not supported"),
            "expected explicit unsupported error, got: {}",
            err.0
        );
    }
}
