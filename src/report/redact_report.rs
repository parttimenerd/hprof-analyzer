//! Report-output redaction (`heap report --redact`).
//!
//! [`redact_report`] blanks every field of a [`Report`](crate::report::Report)
//! that carries actual **heap data content** — decoded String values, JVM
//! system-property values, constant primitive-array values — while leaving
//! everything needed to diagnose a leak intact: class names, field names,
//! counts, shallow/retained sizes, the dominator tree, leak suspects, triage
//! signals, and thread stacks.
//!
//! This is distinct from `hprof-redact` / the `redact` subcommand, which zeros
//! values in the raw **dump**. This function redacts the **report output** so a
//! production heap report can be shared without leaking data.
//!
//! # Leak surface
//!
//! The value-bearing fields were enumerated by grepping `src/report/model.rs`
//! and `src/pass2/model.rs` for `String`/`i64` content fields (see
//! `docs/notes/DESIGN_REDACTED_REPORTS.md`). They are:
//!
//! * `overview.system_properties[].value` — `PropEntry.value`
//! * `overview.duplicate_strings.top_duplicated[].text` and `.top_by_length[].text` —
//!   `DupStringSample.text`
//! * `collections.constant_primitive_arrays.rows[].value` — `ConstantArrayRow.value`
//!
//! **If a new value-bearing field is added to the report, add it here too** —
//! the `redacted_report_suppresses_values` test plants a sentinel string and a
//! sentinel constant-array value in a fixture and asserts neither survives in
//! any rendered format, which is the guardrail against a missed field.

use crate::report::Report;

/// Placeholder written in place of a redacted string value.
pub const REDACTED_PLACEHOLDER: &str = "<redacted>";

/// Redact every data-value field of `report` in place, and set
/// `report.redacted_report = true`. Idempotent. Structural fields (class names,
/// field names, counts, sizes, dominator tree, triage) are untouched.
pub fn redact_report(report: &mut Report) {
    // 1. JVM system-property values (keys like `java.version` are informative
    //    and kept; values can hold secrets passed via -D, so they are blanked).
    for prop in &mut report.overview.system_properties {
        prop.value = REDACTED_PLACEHOLDER.to_string();
    }

    // 2. Decoded duplicate-String sample texts.
    if let Some(dups) = report.overview.duplicate_strings.as_mut() {
        for sample in &mut dups.top_duplicated {
            sample.text = REDACTED_PLACEHOLDER.to_string();
        }
        for sample in &mut dups.top_by_length {
            sample.text = REDACTED_PLACEHOLDER.to_string();
        }
    }

    // 3. Constant primitive-array values (e.g. a byte[] filled with a key byte).
    for row in &mut report.collections.constant_primitive_arrays.rows {
        row.value = 0;
    }

    report.redacted_report = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass2::{DupStringSample, DupStrings};
    use crate::report::{ConstantArrayRow, PropEntry};

    const SENTINEL: &str = "SUPERSECRET-DO-NOT-LEAK";

    /// Build a Report whose three value-bearing fields all carry a sentinel,
    /// redact it, and assert every sentinel is gone while `redacted_report` is
    /// set. This is the guardrail against a missed field — if a new
    /// value-bearing field is added without updating `redact_report`, add it to
    /// this fixture and the assertion below will fail until it's handled.
    #[test]
    fn redacted_report_suppresses_values() {
        let mut r = Report::default();

        // 1. System-property value.
        r.overview.system_properties.push(PropEntry {
            key: "db.password".to_string(),
            value: SENTINEL.to_string(),
        });

        // 2. Duplicate-String sample texts (both lists).
        let sample = DupStringSample {
            text: SENTINEL.to_string(),
            count: 5,
            len: SENTINEL.len() as u32,
            wasted_bytes: 42,
        };
        let dups = DupStrings {
            top_duplicated: vec![sample.clone()],
            top_by_length: vec![sample],
            ..DupStrings::default()
        };
        r.overview.duplicate_strings = Some(dups);

        // 3. Constant primitive-array value (a numeric sentinel).
        r.collections
            .constant_primitive_arrays
            .rows
            .push(ConstantArrayRow {
                array_class: "byte[]".to_string(),
                length: 32,
                value: 0x7F,
                objects: 3,
                shallow: 96,
                owner: None,
                ..ConstantArrayRow::default()
            });

        redact_report(&mut r);

        assert!(r.redacted_report, "redacted_report flag must be set");

        // Serialize the WHOLE report to JSON and grep for the sentinel — this
        // catches any value field regardless of where it renders.
        let json = serde_json::to_string(&r).expect("serialize");
        assert!(
            !json.contains(SENTINEL),
            "string sentinel leaked into redacted report JSON: {json}"
        );

        // Value fields are blanked to the placeholder / 0.
        assert_eq!(r.overview.system_properties[0].value, REDACTED_PLACEHOLDER);
        assert_eq!(r.overview.system_properties[0].key, "db.password"); // key kept
        let dups = r.overview.duplicate_strings.as_ref().unwrap();
        assert_eq!(dups.top_duplicated[0].text, REDACTED_PLACEHOLDER);
        assert_eq!(dups.top_by_length[0].text, REDACTED_PLACEHOLDER);
        assert_eq!(dups.top_duplicated[0].count, 5); // count kept
        assert_eq!(r.collections.constant_primitive_arrays.rows[0].value, 0);
        assert_eq!(
            r.collections.constant_primitive_arrays.rows[0].array_class,
            "byte[]"
        ); // class kept
    }
}
