//! Serde-serializable query results attached to the Report and rendered in
//! md/html/json. Mirrors the spec's QueryResult/QueryValue shapes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryColumn {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", content = "v", rename_all = "snake_case")]
pub enum QueryValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// A reference to a heap object: dense index + its class name for display.
    ObjRef {
        index: u64,
        class: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Optional advisory note (e.g. "edge retention capped"); populated by later
    /// phases, harmless (`None`) otherwise. Omitted from JSON when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_serializes_stably() {
        let r = QueryResult {
            name: "q1".into(),
            oql: "SELECT COUNT(*) FROM C".into(),
            columns: vec![QueryColumn {
                name: "COUNT(*)".into(),
            }],
            rows: vec![vec![QueryValue::Int(42)]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"row_count\":1"));
        assert!(j.contains("\"truncated\":false"));
        assert!(
            !j.contains("\"error\""),
            "error key should be omitted when None: {j}"
        );
        let back: QueryResult = serde_json::from_str(&j).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn query_value_variants_roundtrip() {
        let r = QueryResult {
            name: "variants".into(),
            oql: "SELECT * FROM C".into(),
            columns: vec![QueryColumn { name: "v".into() }],
            rows: vec![vec![
                QueryValue::Null,
                QueryValue::Bool(true),
                QueryValue::Int(-5),
                QueryValue::Float(1.5),
                QueryValue::Str("hi".into()),
                QueryValue::ObjRef {
                    index: 7,
                    class: "java.lang.String".into(),
                },
            ]],
            row_count: 6,
            truncated: false,
            error: None,
            note: None,
        };
        let j = serde_json::to_string(&r).unwrap();
        let back: QueryResult = serde_json::from_str(&j).unwrap();
        assert_eq!(back, r);

        // Verify the tagged enum representation for ObjRef (snake_case -> "obj_ref")
        assert!(
            j.contains("\"kind\":\"obj_ref\""),
            "ObjRef must serialize as kind=obj_ref, got: {j}"
        );
        // Verify Int serializes as "kind":"int"
        assert!(
            j.contains("\"kind\":\"int\""),
            "Int must serialize as kind=int, got: {j}"
        );
        // Verify Bool serializes as "kind":"bool"
        assert!(
            j.contains("\"kind\":\"bool\""),
            "Bool must serialize as kind=bool, got: {j}"
        );
        // ObjRef content should have the "v" key with index and class
        assert!(
            j.contains("\"java.lang.String\""),
            "ObjRef class must appear in JSON"
        );
    }

    #[test]
    fn error_result_roundtrips() {
        let r = QueryResult {
            name: "bad_query".into(),
            oql: "SELECT bad syntax".into(),
            columns: vec![],
            rows: vec![],
            row_count: 0,
            truncated: false,
            error: Some("boom".into()),
            note: None,
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(
            j.contains("\"error\":\"boom\""),
            "error field must appear in JSON, got: {j}"
        );
        let back: QueryResult = serde_json::from_str(&j).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn schema_generates() {
        let schema = schemars::schema_for!(QueryResult);
        let s = serde_json::to_string(&schema).expect("schema must serialize to JSON");
        assert!(!s.is_empty(), "schema JSON must be non-empty");
        assert!(
            s.contains("row_count") || s.contains("QueryResult"),
            "schema JSON must mention row_count or QueryResult, got: {s}"
        );
    }
}
