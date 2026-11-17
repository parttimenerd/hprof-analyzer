//! B6c regression sweep: EVERY SQL example inside the OQL docs
//! (`get_oql_docs`, the same text the MCP `get_oql_docs` tool returns) must
//! parse AND execute without an error result.
//!
//! Routing mirrors the CLI/MCP `run_oql_query` entry: subqueries and VALUES-only
//! queries go through `query::run::run_single_dump` (the two-pass subquery
//! executor — the only path that materializes IN/FROM/EXISTS subplans);
//! everything else goes through the public `run_oql_query` (fast path or
//! auto-escalation to the full pipeline, exactly like `heap query`).
//!
//! Examples carrying placeholder values (`<ClassName>`, `com.example.*`) are
//! substituted with classes present in the fixtures; each example runs against
//! dump_4_philosophers and, when that fixture lacks the referenced classes,
//! against dump_1_mnemonics.

use hprof_analyzer::query::ast::FromSource;
use hprof_analyzer::query::model::QueryResult;
use hprof_analyzer::query::plan::plan_query;
use hprof_analyzer::{AnalyzeOptions, query};

fn extract_sql_blocks(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = md;
    while let Some(start) = rest.find("```sql") {
        let after = &rest[start + 6..];
        let Some(end) = after.find("```") else {
            break;
        };
        out.push(after[..end].to_string());
        rest = &after[end + 3..];
    }
    out
}

#[test]
fn oql_docs_every_example_parses_and_executes() {
    let fixtures = [
        "tests/fixtures/dump_4_philosophers.hprof",
        "tests/fixtures/dump_1_mnemonics.hprof",
    ];
    let docs = hprof_analyzer::mcp::oql_docs::get_oql_docs(None);
    let mut statements: Vec<String> = Vec::new();
    for block in extract_sql_blocks(&docs) {
        let no_comments: String = block
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        for chunk in no_comments.split("\n\n") {
            // A chunk that starts with WITH is ONE statement: its main query
            // begins with a line-initial `SELECT ` that the naive split below
            // would tear off, leaving a lone (unparseable) WITH clause.
            let chunk_is_with = chunk.trim_start().to_ascii_uppercase().starts_with("WITH ");
            let parts: Vec<&str> = if chunk_is_with {
                vec![chunk.trim()]
            } else {
                chunk.trim().split("\nSELECT ").collect()
            };
            for stmt in parts {
                let stmt = stmt.trim();
                // Keep only statements that begin with SELECT/WITH; prose
                // fragments (e.g. WHERE-clause usage lists) are not runnable.
                if !(stmt.starts_with("SELECT") || stmt.starts_with("WITH ")) {
                    continue;
                }
                if stmt.contains("[alias]") || stmt.contains("[WHERE") {
                    continue; // grammar template, not a runnable query
                }
                statements.push(stmt.to_string());
            }
        }
    }
    assert!(
        statements.len() >= 30,
        "doc extraction broke: only {n} statements found",
        n = statements.len()
    );

    let substitute = |s: &str| -> String {
        s.replace("<target_address>", "0x1")
            .replace("<idx>", "0")
            .replace("<ClassName>", "java.lang.String")
    };
    for stmt in &statements {
        let oql = substitute(stmt);
        // Try each fixture; pass if ANY executes without an error result
        // (examples referencing classes the philosophers fixture lacks,
        // e.g. String-heavy queries, run against dump_1).
        let mut last_err: Option<String> = None;
        for fixture in &fixtures {
            if !std::path::Path::new(fixture).exists() {
                continue;
            }
            let q = match query::parse::parse_or_report(&oql) {
                Ok(q) => q,
                Err(report) => {
                    last_err = Some(format!("parse error: {report}"));
                    continue;
                }
            };
            let plan = match plan_query(&q, hprof_analyzer::opts::DEFAULT_QUERY_PATH_DEPTH) {
                Ok(p) => p,
                Err(e) => {
                    last_err = Some(format!("plan error: {}", e.0));
                    continue;
                }
            };
            let plan = query::optimize::optimize(plan, &q, &Default::default());
            // Subquery batches and VALUES-only queries MUST route through
            // run_single_dump (the only executor that materializes subplans);
            // everything else takes the CLI entry point.
            let uses_subqueries = plan.from_subplan.is_some()
                || !plan.in_subplans.is_empty()
                || !plan.exists_subplans.is_empty();
            let values_only = matches!(q.from, FromSource::Values { .. });
            let results: Vec<QueryResult> = if uses_subqueries || values_only {
                query::run::run_single_dump(fixture, &[(q, plan)], true).expect("run_single_dump")
            } else {
                match hprof_analyzer::run_oql_query(fixture, &oql, true, &AnalyzeOptions::default())
                {
                    Ok(r) => r,
                    Err(e) => {
                        last_err = Some(format!("run error: {e}"));
                        continue;
                    }
                }
            };
            let err = results.iter().find_map(|r| r.error.clone());
            if err.is_none() {
                last_err = None;
                break;
            }
            last_err = err;
        }
        assert!(
            last_err.is_none(),
            "documented OQL example failed on all fixtures:\n{oql}\nerror: {}",
            last_err.unwrap_or_default()
        );
    }
}
