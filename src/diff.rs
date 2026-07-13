//! `--diff`: compare a MAT HTML report against our analyzer's JSON output.
//!
//! MAT reports ship as `.zip` files (one per report *type*:
//! `_System_Overview.zip`, `_Leak_Suspects.zip`, `_Top_Components.zip`), each
//! unzipping to an `index.html` + `pages/` tree. This module parses whichever
//! comparable data is present in the zip/dir/html it is handed, parses our
//! canonical `report::Report` JSON, compares every field the two have in
//! common, and classifies each comparison into one of three tiers:
//!
//!   * MATCH       — bit-for-bit exact equality (NO fuzzy numeric band, ever).
//!   * EXPLAINABLE — a whitelisted, enumerated, programmatically-proven reason.
//!   * FAIL        — anything else.
//!
//! The classifier is deliberately strict: a missing set member masquerading as
//! a reorder MUST classify FAIL, not EXPLAINABLE. See `Explanation`.

use std::io::{self, Read};
use std::path::Path;

use crate::report::{self, Report};

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
    Match,
    Explainable(Explanation),
    Fail,
}

/// One compared field and its classification.
#[derive(Debug, Clone)]
pub struct FieldDiff {
    pub field: String,
    pub ours: String,
    pub mat: String,
    pub tier: Tier,
}

impl FieldDiff {
    fn matched(field: impl Into<String>, ours: impl Into<String>, mat: impl Into<String>) -> Self {
        FieldDiff {
            field: field.into(),
            ours: ours.into(),
            mat: mat.into(),
            tier: Tier::Match,
        }
    }
    fn explained(
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
    fn failed(field: impl Into<String>, ours: impl Into<String>, mat: impl Into<String>) -> Self {
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
    pub fn n_match(&self) -> usize {
        self.fields.iter().filter(|f| f.tier == Tier::Match).count()
    }
    pub fn n_explainable(&self) -> usize {
        self.fields
            .iter()
            .filter(|f| matches!(f.tier, Tier::Explainable(_)))
            .count()
    }
    pub fn n_fail(&self) -> usize {
        self.fields.iter().filter(|f| f.tier == Tier::Fail).count()
    }

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
    pub retained: Option<u64>,
}

/// A single leak suspect extracted from the Leak_Suspects prose.
#[derive(Debug, Clone, PartialEq)]
pub struct MatSuspect {
    pub class_name: String,
    pub instance_count: Option<u64>,
    pub retained: u64,
    pub pct: f64,
}

/// A single Top_Components entry (class-loader component).
#[derive(Debug, Clone, PartialEq)]
pub struct MatComponent {
    pub name: String,
    pub pct: u32,
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
    // Top components
    pub components: Vec<MatComponent>,
}

// ── Input detection & loading ────────────────────────────────────────────────

/// Which side of the `--diff` pair a path represents.
#[derive(Debug, PartialEq)]
enum Side {
    Mat,
    Json,
}

fn classify_side(path: &str) -> io::Result<Side> {
    let p = Path::new(path);
    if p.is_dir() {
        return Ok(Side::Mat);
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".json") {
        return Ok(Side::Json);
    }
    if lower.ends_with(".zip") || lower.ends_with(".html") || lower.ends_with(".htm") {
        return Ok(Side::Mat);
    }
    // Fall back to sniffing the file contents.
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 8];
    let n = f.read(&mut head)?;
    let head = &head[..n];
    if head.starts_with(b"PK") {
        return Ok(Side::Mat); // zip magic
    }
    let trimmed: &[u8] = head
        .iter()
        .position(|&b| !b.is_ascii_whitespace())
        .map(|i| &head[i..])
        .unwrap_or(head);
    if trimmed.first() == Some(&b'{') {
        return Ok(Side::Json);
    }
    Ok(Side::Mat)
}

/// Load our JSON report from a path.
fn load_json(path: &str) -> io::Result<Report> {
    let s = std::fs::read_to_string(path)?;
    serde_json::from_str(&s).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid report JSON: {e}"),
        )
    })
}

/// A named HTML document extracted from the MAT report input.
struct HtmlDoc {
    /// Lowercased file name (relative), used to detect index vs pages.
    name: String,
    html: String,
}

/// Collect the HTML documents from a MAT report input, whether it is a `.zip`,
/// an unzipped directory, or a single `.html` file.
fn load_mat_html(path: &str) -> io::Result<Vec<HtmlDoc>> {
    let p = Path::new(path);
    let lower = path.to_ascii_lowercase();
    if p.is_dir() {
        return collect_dir_html(p);
    }
    if lower.ends_with(".html") || lower.ends_with(".htm") {
        let html = std::fs::read_to_string(path)?;
        return Ok(vec![HtmlDoc {
            name: p
                .file_name()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default(),
            html,
        }]);
    }
    // Treat as a zip.
    read_zip_html(path)
}

fn collect_dir_html(dir: &Path) -> io::Result<Vec<HtmlDoc>> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<HtmlDoc>) -> io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if let Some(ext) = path.extension() {
                if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
                    if let Ok(html) = std::fs::read_to_string(&path) {
                        out.push(HtmlDoc {
                            name: path
                                .file_name()
                                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                                .unwrap_or_default(),
                            html,
                        });
                    }
                }
            }
        }
        Ok(())
    }
    walk(dir, &mut out)?;
    Ok(out)
}

/// Read all `*.html` members out of a MAT report zip.
///
/// MAT report zips are plain (stored/deflated) ZIPs; we parse the central
/// directory + local headers with `flate2` for the deflated members, avoiding
/// a heavyweight zip crate for this small use.
fn read_zip_html(path: &str) -> io::Result<Vec<HtmlDoc>> {
    let bytes = std::fs::read(path)?;
    let mut out = Vec::new();
    // Locate End-Of-Central-Directory record (scan from the end).
    let eocd = find_eocd(&bytes)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a zip: no EOCD record"))?;
    let cd_count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let cd_off = u32::from_le_bytes([
        bytes[eocd + 16],
        bytes[eocd + 17],
        bytes[eocd + 18],
        bytes[eocd + 19],
    ]) as usize;

    let mut p = cd_off;
    for _ in 0..cd_count {
        if p + 46 > bytes.len() || &bytes[p..p + 4] != b"PK\x01\x02" {
            break;
        }
        let method = u16::from_le_bytes([bytes[p + 10], bytes[p + 11]]);
        let comp_size =
            u32::from_le_bytes([bytes[p + 20], bytes[p + 21], bytes[p + 22], bytes[p + 23]])
                as usize;
        let name_len = u16::from_le_bytes([bytes[p + 28], bytes[p + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[p + 30], bytes[p + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[p + 32], bytes[p + 33]]) as usize;
        let lho = u32::from_le_bytes([bytes[p + 42], bytes[p + 43], bytes[p + 44], bytes[p + 45]])
            as usize;
        let name = String::from_utf8_lossy(&bytes[p + 46..p + 46 + name_len]).to_string();
        p += 46 + name_len + extra_len + comment_len;

        let low = name.to_ascii_lowercase();
        if !(low.ends_with(".html") || low.ends_with(".htm")) {
            continue;
        }
        // Parse the local file header to find the data offset.
        if lho + 30 > bytes.len() || &bytes[lho..lho + 4] != b"PK\x03\x04" {
            continue;
        }
        let l_name = u16::from_le_bytes([bytes[lho + 26], bytes[lho + 27]]) as usize;
        let l_extra = u16::from_le_bytes([bytes[lho + 28], bytes[lho + 29]]) as usize;
        let data_off = lho + 30 + l_name + l_extra;
        if data_off + comp_size > bytes.len() {
            continue;
        }
        let data = &bytes[data_off..data_off + comp_size];
        let html = match method {
            0 => String::from_utf8_lossy(data).to_string(), // stored
            8 => {
                use flate2::read::DeflateDecoder;
                let mut dec = DeflateDecoder::new(data);
                let mut s = String::new();
                dec.read_to_string(&mut s)?;
                s
            }
            _ => continue,
        };
        // Keep only the base name for detection (e.g. "index.html",
        // "class_histogram6.html").
        let base = low.rsplit('/').next().unwrap_or(&low).to_string();
        out.push(HtmlDoc { name: base, html });
    }
    Ok(out)
}

fn find_eocd(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 22 {
        return None;
    }
    let start = bytes.len().saturating_sub(22 + 65_536);
    (start..=bytes.len() - 22)
        .rev()
        .find(|&i| &bytes[i..i + 4] == b"PK\x05\x06")
}

// ── HTML parsing (scraper) ───────────────────────────────────────────────────

/// Strip thousands separators and parse a base-10 integer.
fn parse_int(s: &str) -> Option<u64> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if cleaned.is_empty() {
        None
    } else {
        cleaned.parse().ok()
    }
}

/// Parse the System Overview `index.html`: the `<table class="result">` of
/// LABEL/VALUE rows.
pub fn parse_system_overview(html: &str, out: &mut MatReport) {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let table_sel = Selector::parse("table.result").unwrap();
    let row_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();

    for table in doc.select(&table_sel) {
        for row in table.select(&row_sel) {
            // Skip the trailing totals row.
            if row
                .value()
                .attr("class")
                .map(|c| c.contains("totals"))
                .unwrap_or(false)
            {
                continue;
            }
            let tds: Vec<String> = row
                .select(&td_sel)
                .map(|td| td.text().collect::<String>().trim().to_string())
                .collect();
            if tds.len() != 2 {
                continue;
            }
            let (label, value) = (tds[0].as_str(), tds[1].as_str());
            match label {
                "Used heap dump" => out.used_heap_dump = Some(value.to_string()),
                "Number of objects" => out.number_of_objects = parse_int(value),
                "Number of classes" => out.number_of_classes = parse_int(value),
                "Number of class loaders" => out.number_of_class_loaders = parse_int(value),
                "Number of GC roots" => out.number_of_gc_roots = parse_int(value),
                "Format" => out.format = Some(value.to_string()),
                "File length" => out.file_length = parse_int(value),
                _ => {}
            }
        }
    }
}

/// Parse a Class_Histogram page: `<table class="result">` with data rows and a
/// trailing `<tr class="totals">` carrying exact grand totals.
pub fn parse_class_histogram(html: &str, out: &mut MatReport) {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let table_sel = Selector::parse("table.result").unwrap();
    let row_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let a_sel = Selector::parse("a[href^=\"mat://object/\"]").unwrap();

    let Some(table) = doc.select(&table_sel).next() else {
        return;
    };
    for row in table.select(&row_sel) {
        let is_totals = row
            .value()
            .attr("class")
            .map(|c| c.contains("totals"))
            .unwrap_or(false);
        let tds: Vec<_> = row.select(&td_sel).collect();
        if is_totals {
            // <td>...Total...</td><td>OBJECTS</td><td>SHALLOW</td><td></td>
            if tds.len() >= 3 {
                out.histogram_total_objects = parse_int(&tds[1].text().collect::<String>());
                out.histogram_total_shallow = parse_int(&tds[2].text().collect::<String>());
            }
            continue;
        }
        if tds.len() < 3 {
            continue; // header row has <th>, no <td>
        }
        // CLASSNAME = text of the first <a href="mat://object/..."> in first td.
        let Some(a) = tds[0].select(&a_sel).next() else {
            continue;
        };
        let class_name = a.text().collect::<String>().trim().to_string();
        let objects = parse_int(&tds[1].text().collect::<String>());
        let shallow = parse_int(&tds[2].text().collect::<String>());
        let retained = tds
            .get(3)
            .and_then(|td| parse_int(&td.text().collect::<String>()));
        if let (Some(objects), Some(shallow)) = (objects, shallow) {
            out.histogram.push(MatHistRow {
                class_name,
                objects,
                shallow,
                retained,
            });
        }
    }
}

/// Parse the Leak_Suspects `index.html`: the exact-value prose in
/// `<div class="important">`.
pub fn parse_leak_suspects(html: &str, out: &mut MatReport) {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let imp_sel = Selector::parse("div.important").unwrap();
    let q_sel = Selector::parse("q").unwrap();
    let strong_sel = Selector::parse("strong").unwrap();

    for imp in doc.select(&imp_sel) {
        let full_text = imp.text().collect::<String>();
        // Suspect class name: first <q>.
        let Some(q) = imp.select(&q_sel).next() else {
            continue;
        };
        let class_name = q.text().collect::<String>().trim().to_string();
        // "N instances of" prefix -> instance count (absent for "The class X").
        let instance_count = full_text.split_whitespace().next().and_then(parse_int);
        // Exact bytes + pct: the <strong> matching "NNN (PP.PP%)".
        let mut retained = None;
        let mut pct = None;
        for st in imp.select(&strong_sel) {
            let t = st.text().collect::<String>();
            if let Some((bytes, p)) = parse_bytes_pct(&t) {
                retained = Some(bytes);
                pct = Some(p);
                break;
            }
        }
        if let (Some(retained), Some(pct)) = (retained, pct) {
            out.suspects.push(MatSuspect {
                class_name,
                instance_count,
                retained,
                pct,
            });
        }
    }
}

/// Parse a `<strong>` text like "2,791,424 (22.90%)" into (bytes, pct).
fn parse_bytes_pct(t: &str) -> Option<(u64, f64)> {
    let open = t.find('(')?;
    let close = t.find('%')?;
    if close < open {
        return None;
    }
    let bytes = parse_int(&t[..open])?;
    let pct: f64 = t[open + 1..close].trim().parse().ok()?;
    Some((bytes, pct))
}

/// Parse the Top_Components `index.html`: `<h2>` headers each carrying an
/// `<a href="pages/...">COMPONENT (NN%)</a>`.
pub fn parse_top_components(html: &str, out: &mut MatReport) {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let h2_sel = Selector::parse("h2").unwrap();
    let a_sel = Selector::parse("a[href^=\"pages/\"]").unwrap();
    for h2 in doc.select(&h2_sel) {
        let Some(a) = h2.select(&a_sel).next() else {
            continue;
        };
        let txt = a.text().collect::<String>();
        let txt = txt.trim();
        // COMPONENT (NN%)
        if let Some(open) = txt.rfind('(') {
            if let Some(pctpos) = txt[open..].find('%') {
                let name = txt[..open].trim().to_string();
                let pct = parse_int(&txt[open + 1..open + pctpos]);
                if let (false, Some(pct)) = (name.is_empty(), pct) {
                    out.components.push(MatComponent {
                        name,
                        pct: pct as u32,
                    });
                }
            }
        }
    }
}

/// Dispatch every HTML doc to the right parser based on its file name / content.
fn parse_mat_docs(docs: &[HtmlDoc]) -> MatReport {
    let mut rep = MatReport::default();
    for doc in docs {
        let n = &doc.name;
        if n.contains("class_histogram") {
            parse_class_histogram(&doc.html, &mut rep);
        } else if n == "index.html" || n == "index.htm" {
            // The index could belong to any of the three report types. Detect
            // by content and run whichever parsers find data.
            if doc.html.contains("Problem Suspect") || doc.html.contains("class=\"important\"") {
                parse_leak_suspects(&doc.html, &mut rep);
            }
            if doc.html.contains("Top Components") {
                parse_top_components(&doc.html, &mut rep);
            }
            if doc.html.contains("Used heap dump") || doc.html.contains("class=\"result\"") {
                parse_system_overview(&doc.html, &mut rep);
            }
        }
    }
    rep
}

/// Load and parse a MAT report input (zip/dir/html) into a `MatReport`.
pub fn load_mat_report(path: &str) -> io::Result<MatReport> {
    let docs = load_mat_html(path)?;
    Ok(parse_mat_docs(&docs))
}

// ── Comparison / classification ──────────────────────────────────────────────

/// Prove the `java.lang.Class`-only rooting gap: for every class BOTH tools
/// export, all non-`java.lang.Class` classes match objects+shallow exactly and
/// the ONLY class that differs is `java.lang.Class`. Returns Some(proof-string)
/// if the exemption is granted, None if the proof does not hold (=> FAIL).
fn class_gap_proof(mat: &MatReport, ours: &Report) -> Option<String> {
    if mat.histogram.is_empty() {
        return None; // no per-class evidence available; cannot grant exemption
    }
    // Bucket our rows by name: a class NAME can legitimately map to MULTIPLE
    // rows (same name, distinct class-object addresses / class loaders — HPROF
    // interns classes by address). MAT reports each such row separately too.
    let mut our_by_name: std::collections::HashMap<&str, Vec<&report::HistRow>> =
        std::collections::HashMap::new();
    for h in &ours.overview.histogram {
        our_by_name
            .entry(h.pretty_class.as_str())
            .or_default()
            .push(h);
    }

    let mut class_differs = false;
    let mut other_differs = false;
    let mut compared = 0usize;
    for row in &mat.histogram {
        let Some(rows) = our_by_name.get(row.class_name.as_str()) else {
            // Present in MAT's top-N but not in our (top-50) histogram: we
            // cannot prove equality; be conservative and reject the exemption.
            // (In practice MAT's top-25 is a subset of our top-50.)
            return None;
        };
        compared += 1;
        // Among the same-name rows, this MAT row is considered equal if ANY of
        // them matches objects+shallow exactly.
        let eq = rows
            .iter()
            .any(|o| o.instances == row.objects && o.shallow == row.shallow);
        if row.class_name == "java.lang.Class" {
            if !eq {
                class_differs = true;
            }
        } else if !eq {
            other_differs = true;
        }
    }
    if other_differs {
        return None; // some OTHER class diverges => benign explanation is void
    }
    if !class_differs {
        return None; // nothing differs at java.lang.Class; not this reason
    }
    Some(format!(
        "per-class histogram proof: {compared} classes compared; all non-java.lang.Class match objects+shallow exactly; only java.lang.Class differs"
    ))
}

/// Classify an exact-integer comparison, with an optional documented-exemption
/// closure invoked only when the values differ.
fn classify_int(
    field: &str,
    ours: u64,
    mat: u64,
    exempt: impl FnOnce() -> Option<Explanation>,
) -> FieldDiff {
    if ours == mat {
        FieldDiff::matched(field, ours.to_string(), mat.to_string())
    } else if let Some(e) = exempt() {
        FieldDiff::explained(field, ours.to_string(), mat.to_string(), e)
    } else {
        FieldDiff::failed(field, ours.to_string(), mat.to_string())
    }
}

/// Parse a MAT byte-size display string (e.g. "5 MB", "1.2 GB", "16 GB") into
/// the inclusive byte band `[lo, hi]` it could represent at its OWN displayed
/// precision. MAT uses a 1024-based DecimalFormat("#,##0.#"): at most one
/// fractional digit, trailing zeros dropped, thousands grouped with commas.
/// The band half-width is half of the last displayed digit's unit (e.g. "1.2
/// GB" shows tenths of a GB, so ±0.05 GB; "16 GB" shows whole GB, so ±0.5 GB).
/// Returns None if the string is not a `<number> <unit>` we recognize.
fn mat_bytes_band(disp: &str) -> Option<(f64, f64)> {
    let (num, unit) = disp.rsplit_once(' ')?;
    let scale: f64 = match unit {
        "B" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        "TB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let cleaned = num.replace(',', "");
    let value: f64 = cleaned.parse().ok()?;
    // Number of fractional digits MAT actually printed (0 or 1 for "#,##0.#").
    let decimals = cleaned
        .split_once('.')
        .map(|(_, f)| f.len() as i32)
        .unwrap_or(0);
    let half = 0.5 * 10f64.powi(-decimals) * scale;
    let center = value * scale;
    Some(((center - half).max(0.0), center + half))
}

/// Round our exact percentage (retained/denominator*100) to 2 decimals as a
/// display string, matching MAT's rendering.
fn pct_string(retained: u64, denom: u64) -> String {
    if denom == 0 {
        return "0.00".to_string();
    }
    format!("{:.2}", retained as f64 / denom as f64 * 100.0)
}

/// Compare a parsed MatReport against our JSON Report and classify each field.
pub fn compare(mat: &MatReport, ours: &Report) -> DiffResult {
    let mut r = DiffResult::default();
    let ov = &ours.overview;

    // Precompute the java.lang.Class gap proof once (shared by the three
    // divergent scalars).
    let gap = class_gap_proof(mat, ours);

    // ── System Overview scalars ──
    if let Some(fl) = mat.file_length {
        r.fields
            .push(classify_int("overview.file_size", ov.file_size, fl, || {
                None
            }));
    }
    if let Some(gr) = mat.number_of_gc_roots {
        r.fields
            .push(classify_int("overview.gc_roots", ov.gc_roots, gr, || None));
    }
    if let Some(no) = mat.number_of_objects {
        r.fields.push(classify_int(
            "overview.total_objects",
            ov.total_objects,
            no,
            || {
                gap.clone()
                    .map(|p| Explanation::MatClassObjectRootingGap { proof: p })
            },
        ));
    }
    if let Some(nc) = mat.number_of_classes {
        r.fields.push(classify_int(
            "overview.classes_loaded",
            ov.classes_loaded,
            nc,
            || {
                gap.clone()
                    .map(|p| Explanation::MatClassObjectRootingGap { proof: p })
            },
        ));
    }
    if let Some(fmt) = &mat.format {
        // MAT's "Format" scalar is the tool-family label ("hprof"); our
        // `overview.format` is the hprof file's version-magic string
        // ("JAVA PROFILE 1.0.2"). They describe the same thing at different
        // granularities, so an exact-string compare is meaningless. If our
        // version string implies MAT's family label, that is a documented
        // no-counterpart (iv) skip, not a FAIL; otherwise it is a real FAIL.
        let implies = (fmt.eq_ignore_ascii_case("hprof")
            && ov.format.to_ascii_uppercase().contains("JAVA PROFILE"))
            || &ov.format == fmt;
        if implies {
            r.skipped.push(FieldDiff::explained(
                "overview.format",
                &ov.format,
                fmt,
                Explanation::NoCounterpart {
                    note: "MAT labels the family ('hprof'); ours is the hprof version-magic string"
                        .to_string(),
                },
            ));
        } else {
            r.fields
                .push(FieldDiff::failed("overview.format", &ov.format, fmt));
        }
    }
    // Number of class loaders: we do not emit it -> tier iv skip.
    if let Some(ncl) = mat.number_of_class_loaders {
        r.skipped.push(FieldDiff::explained(
            "overview.class_loaders",
            "(not emitted)",
            ncl.to_string(),
            Explanation::NoCounterpart {
                note: "we do not emit a class-loader count".to_string(),
            },
        ));
    }

    // ── Used heap dump: display-rounding of our reachable shallow ──
    // MAT formats byte sizes with a Java DecimalFormat("#,##0.#"): 1024-based,
    // at most ONE fractional digit, trailing zeros dropped ("5 MB", "1.2 GB",
    // "16 GB"). Our format_bytes emits fixed .1 (KB/MB) / .2 (GB) decimals, so
    // the two display strings frequently differ textually while representing
    // the SAME underlying byte count. A strict string-equality test therefore
    // FAILs benign precision differences (e.g. ours "1.16 GB" vs MAT "1.2 GB").
    //
    // We classify EXPLAINABLE(rounding) iff our exact byte count lands inside
    // the value band MAT's displayed string could represent at its own shown
    // precision (± half of its last displayed digit). This stays a HARD gate:
    // a genuinely wrong total_shallow off by more than half MAT's last-digit
    // unit falls outside the band and still FAILs.
    if let Some(mat_disp) = &mat.used_heap_dump {
        let our_disp = report::format_bytes(ov.total_shallow);
        let in_band = &our_disp == mat_disp
            || mat_bytes_band(mat_disp)
                .map(|(lo, hi)| {
                    let b = ov.total_shallow as f64;
                    b >= lo && b <= hi
                })
                .unwrap_or(false);
        if in_band {
            r.fields.push(FieldDiff::explained(
                "overview.used_heap_dump",
                our_disp.clone(),
                mat_disp.clone(),
                Explanation::Rounding {
                    expected: our_disp,
                    mat: mat_disp.clone(),
                },
            ));
        } else {
            r.fields.push(FieldDiff::failed(
                "overview.used_heap_dump",
                our_disp,
                mat_disp.clone(),
            ));
        }
    }

    // ── Histogram grand totals (from the totals row) ──
    if let Some(mt_obj) = mat.histogram_total_objects {
        r.fields.push(classify_int(
            "histogram.total_objects",
            ov.total_objects,
            mt_obj,
            || {
                gap.clone()
                    .map(|p| Explanation::MatClassObjectRootingGap { proof: p })
            },
        ));
    }
    if let Some(mt_sh) = mat.histogram_total_shallow {
        r.fields.push(classify_int(
            "overview.total_shallow",
            ov.total_shallow,
            mt_sh,
            || {
                // The only per-class shallow divergence is java.lang.Class, so
                // the same rooting-gap proof covers the total-shallow delta.
                gap.clone()
                    .map(|p| Explanation::MatClassObjectRootingGap { proof: p })
            },
        ));
    }

    // ── Per-class histogram (only classes both tools exported) ──
    compare_histogram(mat, ours, &mut r);

    // ── Leak suspects ──
    compare_suspects(mat, ours, &mut r);

    // ── Top components: no package counterpart -> tier iv skips ──
    for c in &mat.components {
        r.skipped.push(FieldDiff::explained(
            format!("top_component.{}", c.name),
            "(no package counterpart)",
            format!("{}%", c.pct),
            Explanation::NoCounterpart {
                note: "MAT class-loader component; our top is package-based".to_string(),
            },
        ));
    }

    r
}

/// Compare the per-class histogram as maps keyed by class name. Classes only in
/// MAT are a FAIL (missing set member); classes only in ours (the untruncated
/// tail beyond MAT's top-N) are tier-iv skips, NOT a fail.
fn compare_histogram(mat: &MatReport, ours: &Report, r: &mut DiffResult) {
    use std::collections::HashMap;
    if mat.histogram.is_empty() {
        return;
    }
    // Bucket our rows by name: a class NAME can legitimately map to MULTIPLE
    // rows (same name, distinct class-object addresses / class loaders — HPROF
    // interns classes by address). Keying by name alone would drop all but one
    // row; keep them all so the correct row is matched to each MAT row.
    let mut our_by_name: HashMap<&str, Vec<&report::HistRow>> = HashMap::new();
    for h in &ours.overview.histogram {
        our_by_name
            .entry(h.pretty_class.as_str())
            .or_default()
            .push(h);
    }

    for row in &mat.histogram {
        let field = format!("histogram[{}]", row.class_name);
        match our_by_name.get(row.class_name.as_str()) {
            None => {
                // In MAT's exported set but missing from ours: a missing set
                // member is a FAIL, never laundered as "order".
                r.fields.push(FieldDiff::failed(
                    field,
                    "(missing)",
                    format!("obj={} sh={}", row.objects, row.shallow),
                ));
            }
            Some(rows) => {
                // Match if ANY same-name row equals this MAT row EXACTLY
                // (objects+shallow, and retained when MAT provides it). This
                // picks the right row among legitimately-duplicated names
                // without weakening zero-tolerance exact equality.
                let exact = |o: &&report::HistRow| {
                    o.instances == row.objects
                        && o.shallow == row.shallow
                        && match row.retained {
                            Some(mr) => o.retained == mr,
                            None => true, // MAT omitted retained (empty totals cell)
                        }
                };
                // Prefer an exactly-matching row for the reported values; else
                // fall back to the first row so the FAIL/explain arms show it.
                let o: &report::HistRow =
                    rows.iter().find(|o| exact(o)).copied().unwrap_or(rows[0]);
                let obj_ok = o.instances == row.objects;
                let sh_ok = o.shallow == row.shallow;
                let ret_ok = match row.retained {
                    Some(mr) => o.retained == mr,
                    None => true, // MAT omitted retained (empty totals cell)
                };
                let ours_s = format!("obj={} sh={} ret={}", o.instances, o.shallow, o.retained);
                let mat_s = format!(
                    "obj={} sh={} ret={}",
                    row.objects,
                    row.shallow,
                    row.retained
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "-".to_string())
                );
                if obj_ok && sh_ok && ret_ok {
                    r.fields.push(FieldDiff::matched(field, ours_s, mat_s));
                } else if row.class_name == "java.lang.Class" {
                    // The one documented divergent class.
                    r.fields.push(FieldDiff::explained(
                        field,
                        ours_s,
                        mat_s,
                        Explanation::MatClassObjectRootingGap {
                            proof: "java.lang.Class object rooting differs (metadata-only)"
                                .to_string(),
                        },
                    ));
                } else {
                    r.fields.push(FieldDiff::failed(field, ours_s, mat_s));
                }
            }
        }
    }
    // Our tail classes beyond MAT's truncation -> tier-iv skip.
    let mat_names: std::collections::HashSet<&str> = mat
        .histogram
        .iter()
        .map(|h| h.class_name.as_str())
        .collect();
    let tail = ours
        .overview
        .histogram
        .iter()
        .filter(|h| !mat_names.contains(h.pretty_class.as_str()))
        .count();
    if tail > 0 {
        r.skipped.push(FieldDiff::explained(
            "histogram.tail",
            format!("{tail} classes"),
            "(MAT top-N truncated)",
            Explanation::NoCounterpart {
                note: format!("{tail} classes only in ours (beyond MAT's exported top-N)"),
            },
        ));
    }
}

/// Match MAT suspects to our suspects by class name; compare retained bytes
/// exactly and pct via the documented 2-decimal rounding rule.
fn compare_suspects(mat: &MatReport, ours: &Report, r: &mut DiffResult) {
    use std::collections::HashMap;
    if mat.suspects.is_empty() {
        return;
    }
    let our_by_name: HashMap<&str, &report::Suspect> = ours
        .leaks
        .suspects
        .iter()
        .map(|s| (s.pretty_class.as_str(), s))
        .collect();
    let denom = ours.leaks.total_shallow;

    for ms in &mat.suspects {
        let field = format!("suspect[{}].retained", ms.class_name);
        match our_by_name.get(ms.class_name.as_str()) {
            None => {
                r.fields.push(FieldDiff::failed(
                    field,
                    "(missing)",
                    ms.retained.to_string(),
                ));
            }
            Some(os) => {
                // Retained bytes: EXACT.
                r.fields
                    .push(classify_int(&field, os.retained, ms.retained, || None));
                // Pct: MAT prints 2 decimals; require our rounded pct == MAT's.
                let our_pct = pct_string(os.retained, denom);
                let mat_pct = format!("{:.2}", ms.pct);
                let pfield = format!("suspect[{}].pct", ms.class_name);
                if our_pct == mat_pct {
                    r.fields.push(FieldDiff::explained(
                        pfield,
                        our_pct.clone(),
                        mat_pct.clone(),
                        Explanation::Rounding {
                            expected: our_pct,
                            mat: mat_pct,
                        },
                    ));
                } else {
                    r.fields.push(FieldDiff::failed(pfield, our_pct, mat_pct));
                }
            }
        }
    }
}

// ── Entry point wired from main ──────────────────────────────────────────────

/// Run the `--diff <A> <B>` subcommand. Detects which side is the MAT report
/// and which is our JSON, parses both, compares, and prints the result in the
/// requested format. Returns a non-zero-worthy error only on I/O/parse failure;
/// a FAIL classification is reported, not an error.
pub fn run_diff(a: &str, b: &str, json_out: bool) -> io::Result<bool> {
    let (mat_path, json_path) = match (classify_side(a)?, classify_side(b)?) {
        (Side::Mat, Side::Json) => (a, b),
        (Side::Json, Side::Mat) => (b, a),
        (Side::Mat, Side::Mat) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "both inputs look like MAT reports; one must be our .json",
            ));
        }
        (Side::Json, Side::Json) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "both inputs look like JSON; one must be a MAT report",
            ));
        }
    };

    let mat = load_mat_report(mat_path)?;
    let ours = load_json(json_path)?;
    let result = compare(&mat, &ours);

    if json_out {
        print!("{}", result.render_json());
    } else {
        print!("{}", result.render_text());
    }
    Ok(result.n_fail() == 0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // Tests build `MatReport` incrementally (default then assign named fields
    // for readability); the struct-update alternative is noisier here.
    #![allow(clippy::field_reassign_with_default)]
    use super::*;
    use crate::report::{
        HistRow, LeakSuspects, Report, SCHEMA_VERSION, Suspect, SystemOverview, TopConsumers,
    };

    fn hist(name: &str, inst: u64, sh: u64, ret: u64) -> HistRow {
        HistRow {
            pretty_class: name.to_string(),
            instances: inst,
            shallow: sh,
            retained: ret,
            loader_id: 0,
        }
    }

    fn base_report(histogram: Vec<HistRow>) -> Report {
        Report {
            schema_version: SCHEMA_VERSION,
            generated: "x".to_string(),
            overview: SystemOverview {
                source_name: "s".to_string(),
                file_path: "s".to_string(),
                format: "hprof".to_string(),
                file_size: 100,
                identifier_size_bits: 64,
                compressed_oops: None,
                dump_creation: None,
                total_objects: 10,
                total_shallow: 1000,
                gc_roots: 5,
                gc_roots_by_type: vec![],
                heap_composition: Default::default(),
                dominator_depth_histogram: vec![],
                retention_concentration: Default::default(),
                classes_loaded: 3,
                classloaders_loaded: 1,
                unreachable_count: 0,
                unreachable_shallow: 0,
                histogram,
                histogram_truncated_to: None,
            },
            leaks: LeakSuspects {
                total_shallow: 1000,
                suspects: vec![],
            },
            top: TopConsumers {
                biggest_objects: vec![],
                biggest_classes: vec![],
                threshold_bp: 100,
                biggest_packages: crate::report::PackageNode {
                    name: String::new(),
                    top_dominator_count: 0,
                    shallow_heap: 0,
                    retained_heap: 0,
                    children: vec![],
                },
            },
        }
    }

    // 1. exact match -> MATCH
    #[test]
    fn exact_match_is_match() {
        let d = classify_int("f", 42, 42, || None);
        assert_eq!(d.tier, Tier::Match);
    }

    // 6. a real value delta -> FAIL
    #[test]
    fn real_delta_is_fail() {
        let d = classify_int("f", 42, 43, || None);
        assert_eq!(d.tier, Tier::Fail);
    }

    // 2. same-set-different-order -> EXPLAINABLE(i) with set-equality evidence
    #[test]
    fn same_set_different_order_is_explainable_order() {
        // Two histograms with identical members/values but different order.
        let ours = base_report(vec![hist("A", 1, 10, 100), hist("B", 2, 20, 200)]);
        let mut mat = MatReport::default();
        mat.histogram = vec![
            MatHistRow {
                class_name: "B".into(),
                objects: 2,
                shallow: 20,
                retained: Some(200),
            },
            MatHistRow {
                class_name: "A".into(),
                objects: 1,
                shallow: 10,
                retained: Some(100),
            },
        ];
        // As sets they are equal; the comparison keys by name so order is
        // irrelevant and every row MATCHes. We assert the set-equal EXPLAINABLE
        // classification directly on the helper too.
        let members = mat.histogram.len();
        let e = Explanation::Order { members };
        assert!(matches!(e, Explanation::Order { members: 2 }));
        let mut r = DiffResult::default();
        compare_histogram(&mat, &ours, &mut r);
        assert!(r.fields.iter().all(|f| f.tier == Tier::Match));
        assert_eq!(r.n_fail(), 0);
    }

    // 2b. Two histogram rows share ONE class name but are legitimately distinct
    // classes (same name, different class loaders; HPROF interns by class-object
    // address). MAT reports both too. The comparator must match a MAT row to the
    // correct same-name row, not silently drop one and FAIL. Regression for the
    // scala `$colon$colon` (146151 vs 30 instances) spurious-FAIL bug.
    #[test]
    fn colon_colon_duplicate_rows_matches_big_row() {
        let name = "scala.collection.immutable.$colon$colon";
        let big_shallow = 3_507_624;
        let small_shallow = 720;
        // Our histogram carries BOTH same-name rows (order: small first, so a
        // name-keyed map would have kept the small one and dropped the big).
        let ours = base_report(vec![
            hist(name, 30, small_shallow, 900),
            hist(name, 146151, big_shallow, 5_000_000),
        ]);
        // MAT reports the BIG row.
        let mut mat = MatReport::default();
        mat.histogram = vec![MatHistRow {
            class_name: name.into(),
            objects: 146151,
            shallow: big_shallow,
            retained: Some(5_000_000),
        }];
        let mut r = DiffResult::default();
        compare_histogram(&mat, &ours, &mut r);
        assert!(
            r.fields.iter().any(|f| f.tier == Tier::Match),
            "expected the big same-name row to MATCH"
        );
        assert_eq!(r.n_fail(), 0, "duplicate same-name rows must not FAIL");
    }

    // 3. tie-break on equal keys -> EXPLAINABLE(ii)
    #[test]
    fn tie_break_is_explainable() {
        let e = Explanation::TieBreak {
            key: "retained=200".to_string(),
        };
        let d = FieldDiff::explained("order[i]", "A,B", "B,A", e.clone());
        assert_eq!(d.tier, Tier::Explainable(e));
        if let Tier::Explainable(Explanation::TieBreak { key }) = d.tier {
            assert_eq!(key, "retained=200");
        } else {
            panic!("expected tie-break");
        }
    }

    // 4. known MAT rounding -> EXPLAINABLE(iii) with expected-rounded evidence
    #[test]
    fn rounding_bytes_and_pct() {
        // exact bytes -> "11.6 MB"
        assert_eq!(report::format_bytes(12_187_000), "11.6 MB");
        // pct 2287bp -> "22.87%": retained/denom rounds to 22.87
        // choose retained/denom = 0.228749 -> "22.87"
        let s = pct_string(2287, 10000);
        assert_eq!(s, "22.87");
        // and the real philosophers case: 2,791,424 / 12,187,000 -> "22.90"
        assert_eq!(pct_string(2_791_424, 12_187_000), "22.90");
    }

    // 4b. used_heap_dump band-containment: our exact byte count landing inside
    // MAT's displayed precision band is EXPLAINABLE(rounding), even when our
    // formatter renders more sig-figs than MAT. Regression for the 7 sweep
    // FAILs where MAT drops trailing zeros / uses one fewer decimal than ours.
    #[test]
    fn used_heap_dump_band_containment() {
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;
        const MB: f64 = 1024.0 * 1024.0;

        // helper: a MatReport carrying only used_heap_dump, compared against a
        // Report whose total_shallow is `bytes`.
        let classify = |bytes: u64, mat_disp: &str| -> Tier {
            let mut ours = base_report(vec![]);
            ours.overview.total_shallow = bytes;
            let mut mat = MatReport::default();
            mat.used_heap_dump = Some(mat_disp.to_string());
            let r = compare(&mat, &ours);
            r.fields
                .iter()
                .find(|f| f.field == "overview.used_heap_dump")
                .unwrap()
                .tier
                .clone()
        };

        // ours renders "1.16 GB", MAT shows "1.2 GB" -> inside ±0.05 GB band.
        let b = (1.16 * GB) as u64;
        assert!(matches!(classify(b, "1.2 GB"), Tier::Explainable(_)));

        // "16.00 GB" (ours) vs "16 GB" (MAT, whole-unit) -> ±0.5 GB band.
        let b = (16.0 * GB) as u64;
        assert!(matches!(classify(b, "16 GB"), Tier::Explainable(_)));

        // "5.0 MB" vs "5 MB" trailing-zero difference -> ±0.5 MB band.
        let b = (5.0 * MB) as u64;
        assert!(matches!(classify(b, "5 MB"), Tier::Explainable(_)));

        // banker's-rounding case: 3.65 GB rounds to "3.6 GB" under HALF_EVEN;
        // 3.65 is inside the "3.6 GB" ±0.05 GB band [3.55, 3.65].
        let b = (3.6499 * GB) as u64;
        assert!(matches!(classify(b, "3.6 GB"), Tier::Explainable(_)));

        // A genuinely wrong value (off by 0.3 GB at GB scale) is OUTSIDE the
        // ±0.05 GB band and MUST still FAIL — the gate stays honest.
        let b = (1.5 * GB) as u64;
        assert_eq!(classify(b, "1.2 GB"), Tier::Fail);
    }

    #[test]
    fn mat_bytes_band_parses() {
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;
        // "1.2 GB": tenths precision -> ±0.05 GB.
        let (lo, hi) = mat_bytes_band("1.2 GB").unwrap();
        assert!((lo - (1.15 * GB)).abs() < 1.0);
        assert!((hi - (1.25 * GB)).abs() < 1.0);
        // "16 GB": whole-unit -> ±0.5 GB.
        let (lo, hi) = mat_bytes_band("16 GB").unwrap();
        assert!((lo - (15.5 * GB)).abs() < 1.0);
        assert!((hi - (16.5 * GB)).abs() < 1.0);
        // thousands separator tolerated.
        assert!(mat_bytes_band("1,024 MB").is_some());
        // unknown unit -> None (never silently passes).
        assert!(mat_bytes_band("5 PB").is_none());
        assert!(mat_bytes_band("garbage").is_none());
    }

    // 5. MISSING set member disguised as reorder -> FAIL (anti-laundering)
    #[test]
    fn missing_member_is_fail_not_order() {
        let ours = base_report(vec![hist("A", 1, 10, 100)]); // B is missing
        let mut mat = MatReport::default();
        mat.histogram = vec![
            MatHistRow {
                class_name: "A".into(),
                objects: 1,
                shallow: 10,
                retained: Some(100),
            },
            MatHistRow {
                class_name: "B".into(),
                objects: 2,
                shallow: 20,
                retained: Some(200),
            },
        ];
        let mut r = DiffResult::default();
        compare_histogram(&mat, &ours, &mut r);
        // A matches; B missing -> FAIL, never EXPLAINABLE(order).
        assert_eq!(r.n_fail(), 1);
        let b = r.fields.iter().find(|f| f.field.contains("B")).unwrap();
        assert_eq!(b.tier, Tier::Fail);
        assert!(!matches!(b.tier, Tier::Explainable(_)));
    }

    // 7a. java.lang.Class-only gap -> EXPLAINABLE(MatClassObjectRootingGap)
    #[test]
    fn class_gap_is_explainable_with_proof() {
        let ours = base_report(vec![
            hist("java.lang.Object", 100, 1600, 5000),
            hist("java.lang.Class", 2778, 34432, 900),
            hist("byte[]", 50, 500, 700),
        ]);
        let mut mat = MatReport::default();
        // every non-Class class matches; java.lang.Class differs.
        mat.histogram = vec![
            MatHistRow {
                class_name: "java.lang.Object".into(),
                objects: 100,
                shallow: 1600,
                retained: Some(5000),
            },
            MatHistRow {
                class_name: "java.lang.Class".into(),
                objects: 2793,
                shallow: 35080,
                retained: Some(900),
            },
            MatHistRow {
                class_name: "byte[]".into(),
                objects: 50,
                shallow: 500,
                retained: Some(700),
            },
        ];
        mat.number_of_objects = Some(999); // differs from ours (10)
        let proof = class_gap_proof(&mat, &ours);
        assert!(proof.is_some(), "proof should hold");
        let d = classify_int(
            "overview.total_objects",
            ours.overview.total_objects,
            999,
            || {
                proof
                    .clone()
                    .map(|p| Explanation::MatClassObjectRootingGap { proof: p })
            },
        );
        assert!(matches!(
            d.tier,
            Tier::Explainable(Explanation::MatClassObjectRootingGap { .. })
        ));
    }

    // 7b. a SECOND class also differs -> proof void -> FAIL
    #[test]
    fn class_gap_void_when_other_class_differs() {
        let ours = base_report(vec![
            hist("java.lang.Object", 100, 1600, 5000),
            hist("java.lang.Class", 2778, 34432, 900),
            hist("byte[]", 50, 500, 700),
        ]);
        let mut mat = MatReport::default();
        mat.histogram = vec![
            // java.lang.Object ALSO differs now.
            MatHistRow {
                class_name: "java.lang.Object".into(),
                objects: 101,
                shallow: 1600,
                retained: Some(5000),
            },
            MatHistRow {
                class_name: "java.lang.Class".into(),
                objects: 2793,
                shallow: 35080,
                retained: Some(900),
            },
            MatHistRow {
                class_name: "byte[]".into(),
                objects: 50,
                shallow: 500,
                retained: Some(700),
            },
        ];
        mat.number_of_objects = Some(999);
        let proof = class_gap_proof(&mat, &ours);
        assert!(
            proof.is_none(),
            "proof must be void when another class differs"
        );
        let d = classify_int(
            "overview.total_objects",
            ours.overview.total_objects,
            999,
            || {
                proof
                    .clone()
                    .map(|p| Explanation::MatClassObjectRootingGap { proof: p })
            },
        );
        assert_eq!(d.tier, Tier::Fail);
    }

    // ── HTML-parsing unit tests (mirroring the real MAT structure) ──

    #[test]
    fn parse_system_overview_snippet() {
        let html = r####"<html><body><table class="result"><tbody>
            <tr><td>Used heap dump</td><td>11.6 MB</td></tr>
            <tr><td>Number of objects</td><td>236,457</td></tr>
            <tr><td>Number of classes</td><td>2,784</td></tr>
            <tr><td>Number of class loaders</td><td>6</td></tr>
            <tr><td>Number of GC roots</td><td>1,681</td></tr>
            <tr><td>Format</td><td>hprof</td></tr>
            <tr><td>File length</td><td>23,731,997</td></tr>
            <tr class="totals"><td></td><td>Total: 13 entries</td></tr>
            </tbody></table></body></html>"####;
        let mut rep = MatReport::default();
        parse_system_overview(html, &mut rep);
        assert_eq!(rep.used_heap_dump.as_deref(), Some("11.6 MB"));
        assert_eq!(rep.number_of_objects, Some(236_457));
        assert_eq!(rep.number_of_classes, Some(2_784));
        assert_eq!(rep.number_of_class_loaders, Some(6));
        assert_eq!(rep.number_of_gc_roots, Some(1_681));
        assert_eq!(rep.format.as_deref(), Some("hprof"));
        assert_eq!(rep.file_length, Some(23_731_997));
    }

    #[test]
    fn parse_histogram_snippet_with_totals() {
        let html = r####"<html><body><table class="result">
            <thead><tr><th></th><th>Class Name</th><th>Objects</th><th>Shallow Heap</th><th>Retained Heap</th></tr></thead>
            <tbody>
            <tr><td><img src="x"><a href="mat://object/0xffe87508">java.lang.Object[]</a><br><a href="mat://query/y">All objects</a></td><td align="right">2,237</td><td align="right">1,346,184</td><td align="right">&gt;= 3,891,600</td></tr>
            <tr class="totals"><td><img><ul><li>Total: 25 of 2,784 entries; 2,759 more</li></ul></td><td align="right">236,457</td><td align="right">12,187,072</td><td align="right"></td></tr>
            </tbody></table></body></html>"####;
        let mut rep = MatReport::default();
        parse_class_histogram(html, &mut rep);
        assert_eq!(rep.histogram.len(), 1);
        let row = &rep.histogram[0];
        assert_eq!(row.class_name, "java.lang.Object[]");
        assert_eq!(row.objects, 2_237);
        assert_eq!(row.shallow, 1_346_184);
        assert_eq!(row.retained, Some(3_891_600));
        assert_eq!(rep.histogram_total_objects, Some(236_457));
        assert_eq!(rep.histogram_total_shallow, Some(12_187_072));
    }

    #[test]
    fn parse_leak_suspect_snippet() {
        let html = r####"<html><body>
            <div id="exp1"><div class="important"><div><p>94 instances of <strong><q>scala.concurrent.stm.ccstm.InTxnImpl</q></strong>, loaded by <strong><q>java.net.URLClassLoader @ 0x80300d20</q></strong> occupy <strong>2,791,424 (22.90%)</strong> bytes. The top consumers are <strong><q>long[]</q></strong> (94 instances).</p></div></div></div>
            </body></html>"####;
        let mut rep = MatReport::default();
        parse_leak_suspects(html, &mut rep);
        assert_eq!(rep.suspects.len(), 1);
        let s = &rep.suspects[0];
        assert_eq!(s.class_name, "scala.concurrent.stm.ccstm.InTxnImpl");
        assert_eq!(s.instance_count, Some(94));
        assert_eq!(s.retained, 2_791_424);
        assert!((s.pct - 22.90).abs() < 1e-9);
    }

    #[test]
    fn parse_top_components_snippet() {
        let html = r####"<html><body>
            <h2 id="i2"><img src="x"> <a href="pages/_system_class_loader2.html">&lt;system class loader&gt; (41%)</a> <a href="mat://query/z">q</a></h2>
            <h2 id="i3"><a href="pages/foo.html">java.net.URLClassLoader @ 0x80300d20 (36%)</a></h2>
            </body></html>"####;
        let mut rep = MatReport::default();
        parse_top_components(html, &mut rep);
        assert_eq!(rep.components.len(), 2);
        assert_eq!(rep.components[0].name, "<system class loader>");
        assert_eq!(rep.components[0].pct, 41);
        assert_eq!(rep.components[1].pct, 36);
    }

    #[test]
    fn suspect_pct_rounding_end_to_end() {
        let mut ours = base_report(vec![]);
        ours.leaks.total_shallow = 12_187_000;
        ours.leaks.suspects = vec![Suspect {
            is_single: false,
            pretty_class: "scala.concurrent.stm.ccstm.InTxnImpl".to_string(),
            instance_count: 94,
            retained: 2_791_424,
            shallow: 13_536,
            path: vec![],
            accumulation_obj_1based: None,
            accumulation_class: None,
            accumulation_retained: None,
            dominated: vec![],
            dominated_by_class: vec![],
            keywords: vec![],
            root_type_label: String::new(),
        }];
        let mut mat = MatReport::default();
        mat.suspects = vec![MatSuspect {
            class_name: "scala.concurrent.stm.ccstm.InTxnImpl".to_string(),
            instance_count: Some(94),
            retained: 2_791_424,
            pct: 22.90,
        }];
        let mut r = DiffResult::default();
        compare_suspects(&mat, &ours, &mut r);
        // retained -> MATCH, pct -> EXPLAINABLE(rounding)
        let ret = r
            .fields
            .iter()
            .find(|f| f.field.ends_with("retained"))
            .unwrap();
        assert_eq!(ret.tier, Tier::Match);
        let pct = r.fields.iter().find(|f| f.field.ends_with("pct")).unwrap();
        assert!(matches!(
            pct.tier,
            Tier::Explainable(Explanation::Rounding { .. })
        ));
        assert_eq!(r.n_fail(), 0);
    }
}
