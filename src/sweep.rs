//! `--diff-sweep-aggregate`: aggregate many per-dump `--diff --format=json`
//! outputs into a single validation-sweep report + hard-gate verdict.
//!
//! The sweep ORCHESTRATION (running the analyzer on each dump, then `--diff`)
//! lives in `scripts/mat_diff_sweep.sh`; this module only aggregates the
//! per-dump diff JSON files that script produces, so the pass/fail VERDICT
//! logic is unit-testable in isolation (see the tests at the bottom).
//!
//! Gate (see DESIGN decision 11, zero-tolerance): **GATE PASS** iff BOTH
//!   (a) zero FAILs across every dump, AND
//!   (b) the number of REAL MAT comparisons (dumps that had a reference and
//!       produced at least one compared field) >= `N_MIN`.
//! Otherwise **GATE FAIL** with the reason. There is deliberately NO tolerance
//! band or whitelist here: a FAIL is a FAIL.

use std::io;
use std::path::Path;

use serde::Deserialize;

/// The gate minimum: how many real MAT comparisons the sweep must achieve.
pub const N_MIN: usize = 15;

// ── Per-dump diff JSON shape (mirrors `diff::DiffResult::render_json`) ────────

#[derive(Debug, Clone, Deserialize)]
struct DiffField {
    field: String,
    ours: String,
    mat: String,
    tier: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    evidence: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DiffSummary {
    #[serde(rename = "match")]
    n_match: usize,
    explainable: usize,
    fail: usize,
    skip: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct DiffJson {
    fields: Vec<DiffField>,
    // Parsed for shape-completeness; skip counts come from `summary.skip`, so
    // the deserialized member list itself is not walked during aggregation.
    #[serde(default)]
    #[allow(dead_code)]
    skipped: Vec<DiffField>,
    summary: DiffSummary,
}

// ── Aggregated model ─────────────────────────────────────────────────────────

/// One dump's contribution to the sweep.
#[derive(Debug, Clone)]
pub struct DumpTally {
    pub name: String,
    pub n_match: usize,
    pub n_explainable: usize,
    pub n_fail: usize,
    pub n_skip: usize,
    /// EXPLAINABLE hits as (field, reason, evidence, ours, mat) for the audit.
    pub explainable: Vec<(String, String, String, String, String)>,
    /// FAIL hits as (field, ours, mat) for the audit.
    pub fails: Vec<(String, String, String)>,
}

impl DumpTally {
    /// A dump is a REAL comparison if it produced at least one compared field
    /// (match/explainable/fail). A reference that parsed to nothing comparable
    /// does not count toward the gate minimum.
    fn is_real_comparison(&self) -> bool {
        self.n_match + self.n_explainable + self.n_fail > 0
    }
}

/// The aggregate of a whole sweep.
#[derive(Debug, Default, Clone)]
pub struct SweepReport {
    pub dumps: Vec<DumpTally>,
}

impl SweepReport {
    /// Total FAIL count summed across every dump in the sweep.
    pub fn total_fail(&self) -> usize {
        self.dumps.iter().map(|d| d.n_fail).sum()
    }
    /// Number of dumps that were a real (non-skipped) MAT-vs-ours comparison.
    pub fn real_comparisons(&self) -> usize {
        self.dumps.iter().filter(|d| d.is_real_comparison()).count()
    }
    /// GATE PASS iff zero FAILs AND real comparisons >= N_MIN.
    pub fn gate_pass(&self) -> bool {
        self.total_fail() == 0 && self.real_comparisons() >= N_MIN
    }
    /// Human-readable reason for the current verdict.
    pub fn verdict_reason(&self) -> String {
        let fails = self.total_fail();
        let real = self.real_comparisons();
        match (fails == 0, real >= N_MIN) {
            (true, true) => format!("zero FAILs and {real} real comparisons (>= {N_MIN})"),
            (false, true) => format!("{fails} FAIL(s) across all dumps"),
            (true, false) => format!("only {real} real comparisons (< {N_MIN})"),
            (false, false) => {
                format!("{fails} FAIL(s) and only {real} real comparisons (< {N_MIN})")
            }
        }
    }

    fn from_diffs(entries: Vec<(String, DiffJson)>) -> Self {
        let mut report = SweepReport::default();
        for (name, dj) in entries {
            let mut explainable = Vec::new();
            let mut fails = Vec::new();
            for f in &dj.fields {
                match f.tier.as_str() {
                    "EXPLAINABLE" => explainable.push((
                        f.field.clone(),
                        f.reason.clone(),
                        f.evidence.clone(),
                        f.ours.clone(),
                        f.mat.clone(),
                    )),
                    "FAIL" => fails.push((f.field.clone(), f.ours.clone(), f.mat.clone())),
                    _ => {}
                }
            }
            report.dumps.push(DumpTally {
                name,
                n_match: dj.summary.n_match,
                n_explainable: dj.summary.explainable,
                n_fail: dj.summary.fail,
                n_skip: dj.summary.skip,
                explainable,
                fails,
            });
        }
        report.dumps.sort_by(|a, b| a.name.cmp(&b.name));
        report
    }

    /// Render the full sweep report: per-dump table, real-comparison count vs
    /// N_MIN, the full EXPLAINABLE audit list with evidence, the full FAIL
    /// list, and the final verdict line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("=== MAT diff sweep (validation gate) ===\n\n");

        // Per-dump table.
        out.push_str(&format!(
            "{:<34} {:>7} {:>12} {:>6} {:>6}\n",
            "dump", "MATCH", "EXPLAINABLE", "FAIL", "SKIP"
        ));
        out.push_str(&format!("{}\n", "-".repeat(68)));
        for d in &self.dumps {
            out.push_str(&format!(
                "{:<34} {:>7} {:>12} {:>6} {:>6}\n",
                truncate(&d.name, 34),
                d.n_match,
                d.n_explainable,
                d.n_fail,
                d.n_skip,
            ));
        }
        out.push_str(&format!("{}\n", "-".repeat(68)));
        let tot_m: usize = self.dumps.iter().map(|d| d.n_match).sum();
        let tot_e: usize = self.dumps.iter().map(|d| d.n_explainable).sum();
        let tot_f: usize = self.dumps.iter().map(|d| d.n_fail).sum();
        let tot_s: usize = self.dumps.iter().map(|d| d.n_skip).sum();
        out.push_str(&format!(
            "{:<34} {:>7} {:>12} {:>6} {:>6}\n\n",
            "TOTAL", tot_m, tot_e, tot_f, tot_s
        ));

        // Real comparisons vs gate minimum.
        let real = self.real_comparisons();
        out.push_str(&format!(
            "real MAT comparisons: {} (N_MIN = {})  ->  {}\n\n",
            real,
            N_MIN,
            if real >= N_MIN {
                "meets minimum"
            } else {
                "BELOW minimum"
            }
        ));

        // Full EXPLAINABLE audit list with evidence.
        out.push_str("-- EXPLAINABLE hits (audit each proof) --\n");
        let mut any_e = false;
        for d in &self.dumps {
            for (field, reason, evidence, ours, mat) in &d.explainable {
                any_e = true;
                out.push_str(&format!(
                    "  [{}] {}  ours={} mat={}\n      reason={} evidence={}\n",
                    d.name, field, ours, mat, reason, evidence
                ));
            }
        }
        if !any_e {
            out.push_str("  (none)\n");
        }
        out.push('\n');

        // Full FAIL list.
        out.push_str("-- FAIL hits (field, ours, mat) --\n");
        let mut any_f = false;
        for d in &self.dumps {
            for (field, ours, mat) in &d.fails {
                any_f = true;
                out.push_str(&format!(
                    "  [{}] {}  ours={} mat={}\n",
                    d.name, field, ours, mat
                ));
            }
        }
        if !any_f {
            out.push_str("  (none)\n");
        }
        out.push('\n');

        // Final verdict.
        let verdict = if self.gate_pass() {
            "GATE PASS"
        } else {
            "GATE FAIL"
        };
        out.push_str(&format!("{}: {}\n", verdict, self.verdict_reason()));
        out
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let budget = max.saturating_sub(3);
        let end = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= budget)
            .last()
            .unwrap_or(0);
        format!("{}...", &s[..end])
    }
}

/// Read every `*.diff.json` file in `dir`, aggregate, print the report, and
/// return whether the gate passed. Missing/unreadable directory is an I/O
/// error; an empty directory yields an (honest) GATE FAIL for too few
/// comparisons, not an error.
pub fn run_aggregate(dir: &str) -> io::Result<bool> {
    let path = Path::new(dir);
    let mut entries: Vec<(String, DiffJson)> = Vec::new();
    for ent in std::fs::read_dir(path)? {
        let ent = ent?;
        let p = ent.path();
        let fname = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if !fname.ends_with(".diff.json") {
            continue;
        }
        let dump_name = fname.trim_end_matches(".diff.json").to_string();
        let text = std::fs::read_to_string(&p)?;
        let dj: DiffJson = serde_json::from_str(&text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid diff JSON in {}: {e}", p.display()),
            )
        })?;
        entries.push((dump_name, dj));
    }
    let report = SweepReport::from_diffs(entries);
    print!("{}", report.render());
    Ok(report.gate_pass())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tally(name: &str, m: usize, e: usize, f: usize, s: usize) -> DumpTally {
        DumpTally {
            name: name.to_string(),
            n_match: m,
            n_explainable: e,
            n_fail: f,
            n_skip: s,
            explainable: Vec::new(),
            fails: Vec::new(),
        }
    }

    /// zero FAILs + >= N_MIN real comparisons -> GATE PASS.
    #[test]
    fn zero_fail_and_enough_comparisons_passes() {
        let mut r = SweepReport::default();
        for i in 0..N_MIN {
            r.dumps.push(tally(&format!("d{i}"), 5, 1, 0, 2));
        }
        assert_eq!(r.total_fail(), 0);
        assert_eq!(r.real_comparisons(), N_MIN);
        assert!(r.gate_pass());
    }

    /// `truncate` must cut on a char boundary, never panic mid-UTF-8.
    #[test]
    fn truncate_respects_utf8_boundaries() {
        // Short ASCII passes through unchanged.
        assert_eq!(truncate("abc", 34), "abc");
        // A string of multi-byte chars longer than the cap: must not panic and
        // must stay valid UTF-8 with the "..." suffix.
        let s = "héllo wörld ".repeat(10); // each of é/ö is 2 bytes
        let out = truncate(&s, 34);
        assert!(out.ends_with("..."));
        assert!(out.len() <= 34);
        assert!(s.starts_with(out.trim_end_matches("...")));
    }

    /// Any FAIL -> GATE FAIL, even with plenty of comparisons.
    #[test]
    fn any_fail_fails_gate() {
        let mut r = SweepReport::default();
        for i in 0..N_MIN {
            r.dumps.push(tally(&format!("d{i}"), 5, 1, 0, 2));
        }
        // One dump has a single FAIL.
        r.dumps[0].n_fail = 1;
        assert_eq!(r.total_fail(), 1);
        assert!(r.real_comparisons() >= N_MIN);
        assert!(!r.gate_pass());
        assert!(r.verdict_reason().contains("FAIL"));
    }

    /// Fewer than N_MIN real comparisons -> GATE FAIL even with zero FAILs.
    #[test]
    fn too_few_comparisons_fails_gate() {
        let mut r = SweepReport::default();
        for i in 0..(N_MIN - 1) {
            r.dumps.push(tally(&format!("d{i}"), 5, 1, 0, 2));
        }
        assert_eq!(r.total_fail(), 0);
        assert_eq!(r.real_comparisons(), N_MIN - 1);
        assert!(!r.gate_pass());
        assert!(r.verdict_reason().contains("real comparisons"));
    }

    /// A dump with only skips (no compared fields) does NOT count as a real
    /// comparison toward the gate minimum.
    #[test]
    fn skip_only_dump_is_not_a_real_comparison() {
        let mut r = SweepReport::default();
        // N_MIN dumps but one of them is skip-only.
        for i in 0..(N_MIN - 1) {
            r.dumps.push(tally(&format!("d{i}"), 5, 1, 0, 2));
        }
        r.dumps.push(tally("skip_only", 0, 0, 0, 3));
        assert_eq!(r.real_comparisons(), N_MIN - 1);
        assert!(!r.gate_pass());
    }

    /// Both conditions failing reports both reasons.
    #[test]
    fn both_conditions_failing_reports_both() {
        let mut r = SweepReport::default();
        r.dumps.push(tally("d0", 5, 1, 2, 2));
        assert!(!r.gate_pass());
        let reason = r.verdict_reason();
        assert!(reason.contains("FAIL"));
        assert!(reason.contains("real comparisons"));
    }

    /// End-to-end: parse the real diff JSON shape and aggregate it, preserving
    /// the EXPLAINABLE evidence and FAIL details for the audit lists.
    #[test]
    fn parses_real_diff_json_shape() {
        let json = r#"{
  "fields": [
    {"field":"overview.gc_roots","ours":"5","mat":"5","tier":"MATCH","reason":"","evidence":""},
    {"field":"overview.total_objects","ours":"10","mat":"12","tier":"EXPLAINABLE","reason":"MatClassObjectRootingGap","evidence":"per-class proof holds"},
    {"field":"histogram[foo.Bar]","ours":"obj=1 sh=2 ret=3","mat":"obj=1 sh=2 ret=9","tier":"FAIL","reason":"","evidence":""}
  ],
  "skipped": [
    {"field":"overview.class_loaders","ours":"(not emitted)","mat":"6","tier":"EXPLAINABLE","reason":"no-counterpart(iv)","evidence":"we do not emit a class-loader count"}
  ],
  "summary": {"match": 1, "explainable": 1, "fail": 1, "skip": 1}
}"#;
        let dj: DiffJson = serde_json::from_str(json).unwrap();
        let report = SweepReport::from_diffs(vec![("dump_x".to_string(), dj)]);
        assert_eq!(report.dumps.len(), 1);
        let d = &report.dumps[0];
        assert_eq!(d.n_match, 1);
        assert_eq!(d.n_explainable, 1);
        assert_eq!(d.n_fail, 1);
        assert_eq!(d.n_skip, 1);
        assert!(d.is_real_comparison());
        // EXPLAINABLE audit captured with evidence.
        assert_eq!(d.explainable.len(), 1);
        assert_eq!(d.explainable[0].0, "overview.total_objects");
        assert_eq!(d.explainable[0].2, "per-class proof holds");
        // FAIL captured with both values.
        assert_eq!(d.fails.len(), 1);
        assert_eq!(d.fails[0].0, "histogram[foo.Bar]");
        assert_eq!(d.fails[0].1, "obj=1 sh=2 ret=3");
        assert_eq!(d.fails[0].2, "obj=1 sh=2 ret=9");
        // One dump -> below the gate minimum, and it has a FAIL.
        assert!(!report.gate_pass());
        // Rendering includes the verdict line and the audit sections.
        let rendered = report.render();
        assert!(rendered.contains("GATE FAIL"));
        assert!(rendered.contains("per-class proof holds"));
        assert!(rendered.contains("histogram[foo.Bar]"));
    }
}
