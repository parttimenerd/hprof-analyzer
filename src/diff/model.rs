//! Diff data model: tier/explanation classification and the parsed-MAT-report
//! structs. Pure data types plus their small render/construct impls; no I/O or
//! comparison logic. Byte-for-byte identical to the pre-split `diff.rs`.

// ── Tier / explanation model ────────────────────────────────────────────────

/// The enumerated, whitelisted reasons a non-exact comparison may still be
/// considered EXPLAINABLE. Every variant carries the *evidence* proving it.
#[derive(Debug, Clone, PartialEq)]
pub enum Explanation {
    /// (i) Traversal/iteration ORDER differs, but the two collections are
    /// equal AS SETS (same members, same per-member values). Part of the
    /// enumerated whitelist; exercised by the classifier tests. Our runtime
    /// per-class comparison keys by name and so is order-agnostic, hence this
    /// variant is only constructed in tests today.
    #[cfg_attr(not(test), allow(dead_code))]
    Order { members: usize },
    /// (ii) Stable-sort tie-break on entries that have IDENTICAL sort keys.
    /// Enumerated whitelist reason; exercised by tests.
    #[cfg_attr(not(test), allow(dead_code))]
    TieBreak { key: String },
    /// (iii) MAT display rounding / unit truncation: our exact value renders to
    /// exactly the string MAT displayed.
    Rounding { expected: String, mat: String },
    /// (iv) MAT-only or ours-only field with NO counterpart — skipped.
    NoCounterpart { note: String },
    /// (special) The `total_objects` / `classes_loaded` / `total_shallow`
    /// divergence proven to be localized entirely to `java.lang.Class` object
    /// reachability. Only valid when the per-class histogram proof holds.
    MatClassObjectRootingGap { proof: String },
}

impl Explanation {
    fn label(&self) -> &'static str {
        match self {
            Explanation::Order { .. } => "order(i)",
            Explanation::TieBreak { .. } => "tie-break(ii)",
            Explanation::Rounding { .. } => "rounding(iii)",
            Explanation::NoCounterpart { .. } => "no-counterpart(iv)",
            Explanation::MatClassObjectRootingGap { .. } => "MatClassObjectRootingGap",
        }
    }
    fn evidence(&self) -> String {
        match self {
            Explanation::Order { members } => {
                format!("set-equal, {members} members, order differs")
            }
            Explanation::TieBreak { key } => format!("identical sort key: {key}"),
            Explanation::Rounding { expected, mat } => {
                format!("our value renders '{expected}' == MAT '{mat}'")
            }
            Explanation::NoCounterpart { note } => note.clone(),
            Explanation::MatClassObjectRootingGap { proof } => proof.clone(),
        }
    }
}

/// The 3-tier classification of one compared field.
#[derive(Debug, Clone, PartialEq)]
pub enum Tier {
    /// Bit-for-bit exact equality.
    Match,
    /// Non-exact, but carries a whitelisted proof (see `Explanation`).
    Explainable(Explanation),
    /// Any divergence without a whitelisted proof.
    Fail,
}

/// One compared field and its classification.
#[derive(Debug, Clone)]
pub struct FieldDiff {
    /// Dotted field identifier (e.g. `histogram[java.lang.Object]`).
    pub field: String,
    /// Our side's value rendered for display.
    pub ours: String,
    /// MAT's side's value rendered for display.
    pub mat: String,
    /// This field's 3-tier classification.
    pub tier: Tier,
}

impl FieldDiff {
    pub(crate) fn matched(
        field: impl Into<String>,
        ours: impl Into<String>,
        mat: impl Into<String>,
    ) -> Self {
        FieldDiff {
            field: field.into(),
            ours: ours.into(),
            mat: mat.into(),
            tier: Tier::Match,
        }
    }
    pub(crate) fn explained(
        field: impl Into<String>,
        ours: impl Into<String>,
        mat: impl Into<String>,
        e: Explanation,
    ) -> Self {
        FieldDiff {
            field: field.into(),
            ours: ours.into(),
            mat: mat.into(),
            tier: Tier::Explainable(e),
        }
    }
    pub(crate) fn failed(
        field: impl Into<String>,
        ours: impl Into<String>,
        mat: impl Into<String>,
    ) -> Self {
        FieldDiff {
            field: field.into(),
            ours: ours.into(),
            mat: mat.into(),
            tier: Tier::Fail,
        }
    }
}

/// The full result of a `--diff` comparison.
#[derive(Debug, Default)]
pub struct DiffResult {
    pub fields: Vec<FieldDiff>,
    /// Fields present on only one side (tier-iv skips), recorded separately so
    /// they never count as FAIL.
    pub skipped: Vec<FieldDiff>,
}

impl DiffResult {
    /// Count of fields classified MATCH.
    pub fn n_match(&self) -> usize {
        self.fields.iter().filter(|f| f.tier == Tier::Match).count()
    }
    /// Count of fields classified EXPLAINABLE.
    pub fn n_explainable(&self) -> usize {
        self.fields
            .iter()
            .filter(|f| matches!(f.tier, Tier::Explainable(_)))
            .count()
    }
    /// Count of fields classified FAIL (a non-zero count fails the diff).
    pub fn n_fail(&self) -> usize {
        self.fields.iter().filter(|f| f.tier == Tier::Fail).count()
    }

    /// Render the diff as a human-readable text table with a summary line.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("=== hprof-analyzer --diff (MAT report vs our JSON) ===\n\n");
        for f in &self.fields {
            let (mark, detail) = match &f.tier {
                Tier::Match => ("MATCH      ".to_string(), String::new()),
                Tier::Explainable(e) => (
                    "EXPLAINABLE".to_string(),
                    format!("  [{}: {}]", e.label(), e.evidence()),
                ),
                Tier::Fail => ("FAIL       ".to_string(), String::new()),
            };
            out.push_str(&format!(
                "  {}  {:<28} ours={:<16} mat={}{}\n",
                mark, f.field, f.ours, f.mat, detail
            ));
        }
        if !self.skipped.is_empty() {
            out.push_str("\n-- skipped (no counterpart, tier iv) --\n");
            for f in &self.skipped {
                let note = match &f.tier {
                    Tier::Explainable(e) => e.evidence(),
                    _ => String::new(),
                };
                out.push_str(&format!(
                    "  SKIP         {:<28} ours={:<16} mat={}  [{}]\n",
                    f.field, f.ours, f.mat, note
                ));
            }
        }
        out.push_str(&format!(
            "\nsummary: MATCH={} EXPLAINABLE={} FAIL={} SKIP={}\n",
            self.n_match(),
            self.n_explainable(),
            self.n_fail(),
            self.skipped.len(),
        ));
        out
    }

    /// Render the diff as machine-readable JSON (fields, skips, summary counts).
    pub fn render_json(&self) -> String {
        // Hand-rolled JSON to avoid deriving serde on the diff types; the shape
        // is small and stable so a sweep script can aggregate it.
        fn esc(s: &str) -> String {
            s.replace('\\', "\\\\").replace('"', "\\\"")
        }
        fn field_json(f: &FieldDiff) -> String {
            let (tier, reason, evidence) = match &f.tier {
                Tier::Match => ("MATCH", String::new(), String::new()),
                Tier::Fail => ("FAIL", String::new(), String::new()),
                Tier::Explainable(e) => ("EXPLAINABLE", e.label().to_string(), e.evidence()),
            };
            format!(
                "{{\"field\":\"{}\",\"ours\":\"{}\",\"mat\":\"{}\",\"tier\":\"{}\",\"reason\":\"{}\",\"evidence\":\"{}\"}}",
                esc(&f.field),
                esc(&f.ours),
                esc(&f.mat),
                tier,
                esc(&reason),
                esc(&evidence),
            )
        }
        let mut out = String::from("{\n  \"fields\": [\n");
        let all: Vec<String> = self.fields.iter().map(field_json).collect();
        out.push_str(
            &all.iter()
                .map(|s| format!("    {s}"))
                .collect::<Vec<_>>()
                .join(",\n"),
        );
        out.push_str("\n  ],\n  \"skipped\": [\n");
        let sk: Vec<String> = self.skipped.iter().map(field_json).collect();
        out.push_str(
            &sk.iter()
                .map(|s| format!("    {s}"))
                .collect::<Vec<_>>()
                .join(",\n"),
        );
        out.push_str(&format!(
            "\n  ],\n  \"summary\": {{\"match\": {}, \"explainable\": {}, \"fail\": {}, \"skip\": {}}}\n}}\n",
            self.n_match(),
            self.n_explainable(),
            self.n_fail(),
            self.skipped.len(),
        ));
        out
    }
}

// ── Parsed MAT report ────────────────────────────────────────────────────────

/// A single class-histogram row extracted from MAT's Class_Histogram page.
#[derive(Debug, Clone, PartialEq)]
pub struct MatHistRow {
    pub class_name: String,
    pub objects: u64,
    pub shallow: u64,
    /// Retained bytes, when MAT's table included the column (it is empty on the
    /// System Overview histogram, so `None` means "not exported", not zero).
    pub retained: Option<u64>,
}

/// A single leak suspect extracted from the Leak_Suspects prose.
#[derive(Debug, Clone, PartialEq)]
pub struct MatSuspect {
    pub class_name: String,
    /// "N instances of" count; `None` for the "The class X" phrasing, `Some(1)`
    /// for the thread variant (a single thread object).
    pub instance_count: Option<u64>,
    pub retained: u64,
    /// Retained % as MAT printed it (2-decimal display value, not exact).
    pub pct: f64,
}

/// A single Top_Components entry (class-loader component).
#[derive(Debug, Clone, PartialEq)]
pub struct MatComponent {
    pub name: String,
    /// Whole-number percentage MAT printed for the component.
    pub pct: u32,
}

/// A "Biggest Objects" row from the Top Consumers page (dominator-tree table).
/// MAT's label carries the object address ("class X @ 0xADDR", "X @ 0xADDR  Name");
/// `class_name` is the normalized bare class name.
#[derive(Debug, Clone, PartialEq)]
pub struct MatBiggestObject {
    pub class_name: String,
    pub shallow: u64,
    pub retained: u64,
}

/// A "Biggest Top-Level Dominator Classes" row from the Top Consumers page.
#[derive(Debug, Clone, PartialEq)]
pub struct MatBiggestClass {
    pub class_name: String,
    pub objects: u64,
    pub retained: u64,
}

/// One row of MAT's "Biggest Top-Level Dominator Packages" tree. `depth` is the
/// nesting level (0 = the `<all>` root). `dotted_path` is the accumulated
/// package path from the root's children down ("java.util.zip"); the root
/// itself has an empty path (mirroring our `PackageNode` root name "").
#[derive(Debug, Clone, PartialEq)]
pub struct MatPackageRow {
    pub depth: usize,
    pub segment: String,
    pub dotted_path: String,
    pub retained: u64,
    /// MAT's "# Top Dominators" for this node — the count of top-level
    /// dominator objects under the package. Load-bearing in `package_gap_proof`:
    /// a divergent java.lang node must show MAT rooting strictly more than us.
    pub top_dominators: u64,
}

/// Everything comparable we managed to extract from whatever zip/dir/html we
/// were handed. All fields are optional; absent data is skipped cleanly.
#[derive(Debug, Default, Clone)]
pub struct MatReport {
    // System Overview scalars
    pub used_heap_dump: Option<String>, // display string, e.g. "11.6 MB"
    pub number_of_objects: Option<u64>,
    pub number_of_classes: Option<u64>,
    pub number_of_class_loaders: Option<u64>,
    pub number_of_gc_roots: Option<u64>,
    pub format: Option<String>,
    pub file_length: Option<u64>,
    // Class histogram (top-N; MAT truncates)
    pub histogram: Vec<MatHistRow>,
    pub histogram_total_objects: Option<u64>,
    pub histogram_total_shallow: Option<u64>,
    // Leak suspects
    pub suspects: Vec<MatSuspect>,
    // Top components (class-loader components from the Top_Components index)
    pub components: Vec<MatComponent>,
    // Top consumers page tables
    pub biggest_objects: Vec<MatBiggestObject>,
    pub biggest_classes: Vec<MatBiggestClass>,
    pub packages: Vec<MatPackageRow>,
}
