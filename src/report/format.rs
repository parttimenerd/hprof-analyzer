//! Formatting, naming, and small display helpers shared by the report
//! builders and renderers (byte-for-byte identical to the pre-split module).

use super::*;
use crate::pass2::Graph;

#[inline]
pub(crate) fn class_obj_repr(g: &Graph, i: usize) -> u32 {
    g.class_obj_class_idx
        .get(&(i as u32))
        .copied()
        .unwrap_or(u32::MAX)
}

/// Human-readable label for an HPROF GC-root sub-tag, used by the
/// GC-roots-by-type breakdown. Mirrors the MAT root-type naming.
pub(crate) fn gc_root_type_label(ty: u8) -> &'static str {
    use crate::types::heap;
    match ty {
        heap::ROOT_SYSTEM_CLASS => "System Class",
        heap::ROOT_JNI_GLOBAL => "JNI Global",
        heap::ROOT_JNI_LOCAL => "JNI Local",
        heap::ROOT_JAVA_FRAME => "Java Frame",
        heap::ROOT_NATIVE_STACK => "Native Stack",
        heap::ROOT_STICKY_CLASS => "Sticky Class",
        heap::ROOT_THREAD_BLOCK => "Thread Block",
        heap::ROOT_MONITOR_USED => "Busy Monitor",
        heap::ROOT_THREAD_OBJ => "Thread",
        _ => "Unknown",
    }
}

/// Escape a decoded String value for a one-line Markdown table code-span cell:
/// collapse newlines/tabs to spaces, escape table pipes, and replace backticks
/// (which would break the surrounding code-span) with single quotes.
pub(crate) fn escape_string_cell(s: &str) -> String {
    s.replace(['\n', '\r', '\t'], " ")
        .replace('|', "\\|")
        .replace('`', "'")
}
/// a single `[` followed by one primitive type char. These are boot-loaded
/// (single loader), so exact-name duplicate rows can be folded safely.
pub(crate) fn is_prim_array_desc(name: &str) -> bool {
    name.len() == 2
        && name.as_bytes()[0] == b'['
        && matches!(
            name.as_bytes()[1],
            b'Z' | b'C' | b'F' | b'D' | b'S' | b'I' | b'J' | b'B'
        )
}

/// Build a per-class-row remap that folds exact-raw-name duplicate histogram
/// rows into a single canonical (lowest-indexed) row, matching MAT's
/// by-object-type histogram semantics.
///
/// Two name families produce duplicate rows that MAT reports as one:
///  - `java/lang/Class`: class objects (`kind==3`) key under a single sentinel
///    row (`JLC_KEY`), but primitive-type Class *mirrors* (`int.class`, …) are
///    parsed as plain instances whose class-object address *is*
///    `java/lang/Class`, landing in a separate same-named row.
///  - primitive-array descriptors (`[B`, `[I`, …): the actual `byte[]`/`int[]`
///    INSTANCES key under `PRIM_KEY_BASE|type_code`, but the instance-less
///    primitive-array CLASS objects (root-attached to mirror MAT's
///    addSystemClassRootsIfMissing) become reachable metadata objects that
///    intern into a *separate* zero-instance row with the same `[B`/`[I` name.
///
/// Only these two families are folded by name. Ordinary instance rows are
/// interned by loader-distinct class-object address, so a class loaded by two
/// loaders legitimately yields two same-name rows that MUST stay separate; we
/// therefore never fold arbitrary same-name rows.
///
/// The returned vector maps `row -> canonical_row`; non-foldable rows map to
/// themselves. Applying it in the histogram and Biggest-Classes tallies
/// re-attributes the duplicates without touching reachability,
/// `classes_loaded`, or `total_objects`.
pub(crate) fn class_row_remap(g: &Graph) -> Vec<u32> {
    let class_count = g.class_names.len();
    let mut remap: Vec<u32> = (0..class_count as u32).collect();
    // First occurrence of each foldable name becomes its canonical row.
    let mut canonical: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for (row, name) in g.class_names.iter().enumerate() {
        if name == "java/lang/Class" || is_prim_array_desc(name) {
            let canon = *canonical.entry(name.as_str()).or_insert(row as u32);
            remap[row] = canon;
        }
    }
    remap
}

/// Human-readable label for a GC-root HPROF sub-tag (see `types::heap::ROOT_*`).
/// Returns `None` for `ROOT_UNKNOWN` and any unrecognised code, so callers can
/// suppress the "held by" clause when the holding root type is not meaningful.
/// Labels follow MAT's GC-root naming.
pub(crate) fn gc_root_type_label_opt(code: u8) -> Option<&'static str> {
    use crate::types::heap;
    match code {
        heap::ROOT_JNI_GLOBAL => Some("JNI Global"),
        heap::ROOT_JNI_LOCAL => Some("JNI Local"),
        heap::ROOT_JAVA_FRAME => Some("Java Frame"),
        heap::ROOT_NATIVE_STACK => Some("Native Stack"),
        heap::ROOT_STICKY_CLASS => Some("Sticky Class"),
        heap::ROOT_THREAD_BLOCK => Some("Thread Block"),
        heap::ROOT_MONITOR_USED => Some("Busy Monitor"),
        heap::ROOT_THREAD_OBJ => Some("Thread"),
        heap::ROOT_SYSTEM_CLASS => Some("System Class"),
        _ => None,
    }
}

// ── Formatting helpers ─────────────────────────────────────────────────────

/// ISO-8601 UTC timestamp matching java.time.Instant.toString() shape.
/// Non-deterministic — parity comparison ignores this line.
pub fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format_epoch_nanos(now.as_secs(), now.subsec_nanos())
}

/// Format a millis-since-Unix-epoch instant as `YYYY-MM-DDTHH:MM:SSZ` (UTC),
/// second granularity. Used for the deterministic dump-creation timestamp;
/// negative (pre-1970) values are clamped to the epoch.
pub fn format_epoch_ms(ms: i64) -> String {
    let secs = if ms < 0 { 0 } else { (ms / 1000) as u64 };
    let full = format_epoch_nanos(secs, 0);
    // full is "...SS.000000000Z"; trim the fractional seconds for readability.
    match (full.find('.'), full.rfind('Z')) {
        (Some(dot), Some(z)) if dot < z => format!("{}{}", &full[..dot], &full[z..]),
        _ => full,
    }
}

/// Core civil-date formatter (Howard Hinnant's algorithm) shared by the
/// now/creation timestamp helpers. Produces `YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`.
fn format_epoch_nanos(secs: u64, nanos: u32) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        year, m, d, hh, mm, ss, nanos
    )
}

/// Human-readable byte size (`B`/`KB`/`MB`/`GB`, binary 1024 base). Used only
/// for display; the JSON model always carries raw `u64` byte counts.
pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{} B", n);
    }
    if n < 1024 * 1024 {
        return format!("{:.1} KB", n as f64 / 1024.0);
    }
    if n < 1024 * 1024 * 1024 {
        return format!("{:.1} MB", n as f64 / (1024.0 * 1024.0));
    }
    format!("{:.2} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// Group an integer into comma-separated thousands (e.g. `1234567` -> `1,234,567`).
pub(crate) fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Plain-language explainer shown under every "Dominator-Depth Distribution"
/// heading. Depth = how many dominator hops an object sits below a GC root, so a
/// tall shallow side (low depths) means most memory is retained close to the
/// roots, while a long tail means deep, chained structures.
pub(crate) const DEPTH_DIST_CAPTION: &str = "_How far each live object sits below a GC \
root, counted in dominator hops. Most objects clustering at shallow depths \
means memory is held close to the roots; a long tail means deep, chained \
structures (often a sign of nested collections or linked leaks)._\n\n";

/// Derived per-bucket depth stats (percent + running cumulative percent) plus a
/// one-line human summary, all computed from the raw `objects` counts. Kept out
/// of the JSON model on purpose: it is fully derivable, so emitting it would
/// bloat the report without adding information. Each row is
/// `(depth, objects, pct_of_total, cumulative_pct)`; percents are 0.0–100.0.
pub(crate) struct DepthStats {
    pub(crate) rows: Vec<(u32, u64, f64, f64)>,
    /// The smallest depth at which the cumulative object share reaches 50%.
    pub(crate) median_depth: u32,
    /// The deepest bucket present (longest dominator chain).
    pub(crate) max_depth: u32,
}

/// Compute [`DepthStats`] from the histogram buckets. Returns `None` when there
/// are no objects at all (nothing meaningful to summarise).
pub(crate) fn depth_stats(hist: &[DepthBucket]) -> Option<DepthStats> {
    let total: u64 = hist.iter().map(|b| b.objects).sum();
    if total == 0 {
        return None;
    }
    let total_f = total as f64;
    let mut rows = Vec::with_capacity(hist.len());
    let mut running: u64 = 0;
    let mut median_depth = hist.last().map(|b| b.depth).unwrap_or(0);
    let mut median_found = false;
    for b in hist {
        running += b.objects;
        let pct = b.objects as f64 / total_f * 100.0;
        let cum = running as f64 / total_f * 100.0;
        if !median_found && running * 2 >= total {
            median_depth = b.depth;
            median_found = true;
        }
        rows.push((b.depth, b.objects, pct, cum));
    }
    let max_depth = hist.last().map(|b| b.depth).unwrap_or(0);
    Some(DepthStats {
        rows,
        median_depth,
        max_depth,
    })
}

/// One-line summary sentence for the depth distribution, e.g. "Half of all live
/// objects sit within 2 hops of a GC root; the deepest chain is 28 hops."
pub(crate) fn depth_summary_line(s: &DepthStats) -> String {
    format!(
        "_Half of all live objects sit within {} hop{} of a GC root; the deepest chain is {} hop{}._\n\n",
        s.median_depth,
        if s.median_depth == 1 { "" } else { "s" },
        s.max_depth,
        if s.max_depth == 1 { "" } else { "s" },
    )
}

/// Format a percentage to one decimal place with a trailing `%`, e.g. `12.3%`.
pub(crate) fn fmt_pct(p: f64) -> String {
    format!("{p:.1}%")
}

/// Convert a JVM internal class descriptor to a display name: `/` -> `.`, and
/// array descriptors (`[I`, `[Ljava/lang/String;`) into `int[]` / `java.lang.String[]`.
pub fn pretty_class_name(raw: &str) -> String {
    if raw.is_empty() {
        return raw.to_string();
    }
    if !raw.starts_with('[') {
        return raw.replace('/', ".");
    }

    let dims = raw.chars().take_while(|&c| c == '[').count();
    let rest = &raw[dims..];

    let base = if rest.len() == 1 {
        match rest.chars().next().unwrap() {
            'Z' => "boolean",
            'B' => "byte",
            'C' => "char",
            'S' => "short",
            'I' => "int",
            'J' => "long",
            'F' => "float",
            'D' => "double",
            _ => rest,
        }
        .to_string()
    } else if rest.starts_with('L') && rest.ends_with(';') {
        rest[1..rest.len() - 1].replace('/', ".")
    } else {
        rest.replace('/', ".")
    };

    format!("{}{}", base, "[]".repeat(dims))
}

/// The 4-way kind of a reachable object, for heap composition (B5). Derives
/// from class-object membership and the raw JVM class-name descriptor — there
/// is no `kind[]` array in Graph. Mirrors `pretty_class_name`'s array parsing:
/// a single `[X` primitive descriptor is a primitive array; any other `[…`
/// (e.g. `[L…;`, `[[B`) is an object array.
pub(crate) fn object_kind(g: &Graph, i: usize) -> &'static str {
    if class_obj_repr(g, i) != u32::MAX {
        return "Class objects";
    }
    let raw = match g.class_names.get(g.class_idx[i] as usize) {
        Some(r) => r,
        None => return "Instances",
    };
    if is_prim_array_desc(raw) {
        "Primitive arrays"
    } else if raw.starts_with('[') {
        "Object arrays"
    } else {
        "Instances"
    }
}

/// The full dotted PACKAGE PATH of a class from its JVM internal name.
///
/// Normalises like the histogram (strip leading `[`, strip `L...;`), then takes
/// everything BEFORE the final `.`. Primitives/arrays collapse to the sentinel
/// `(primitives)`; a class in the default package (no dot) becomes `(default)`.
/// Examples: `java/util/concurrent/Foo` -> `java.util.concurrent`;
/// `Foo` -> `(default)`; `[I` -> `(primitives)`.
pub(crate) fn package_path(name: &str) -> String {
    let mut s = name;
    while s.starts_with('[') {
        s = &s[1..];
    }
    if s.starts_with('L') && s.ends_with(';') {
        s = &s[1..s.len() - 1];
    }
    if s.is_empty() || matches!(s, "B" | "C" | "D" | "F" | "I" | "J" | "S" | "Z") {
        return "(primitives)".to_string();
    }
    if s.ends_with("[]") {
        return "(primitives)".to_string();
    }
    let s = s.replace('/', ".");
    match s.rfind('.') {
        Some(dot) => s[..dot].to_string(),
        None => "(default)".to_string(),
    }
}
