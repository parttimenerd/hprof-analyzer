//! Formatting, naming, and small display helpers shared by the report
//! builders and renderers (byte-for-byte identical to the pre-split module).

use super::*;
use crate::pass2::Graph;

#[inline]
pub(crate) fn class_obj_repr(g: &Graph, i: usize) -> u32 {
    // Fast pre-filter: only class objects have class_idx == jlc_idx (the
    // java/lang/Class row). Avoids a HashMap probe for the ~99.99% of non-class
    // objects. jlc_idx == u32::MAX means "not known" (test fixtures) — falls
    // back to the full HashMap probe in that case.
    if g.jlc_idx != u32::MAX && g.class_idx[i] != g.jlc_idx {
        return u32::MAX;
    }
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
        heap::ROOT_INTERNED_STRING => "Interned String",
        heap::ROOT_DEBUGGER => "Debugger",
        heap::ROOT_VM_INTERNAL => "VM Internal",
        heap::ROOT_JNI_MONITOR => "JNI Monitor",
        _ => "Unknown",
    }
}

/// Escape a decoded String value for a one-line Markdown table code-span cell:
/// collapse newlines/tabs to spaces, escape table pipes, replace backticks
/// (which would break the surrounding code-span) with single quotes, and map
/// any remaining control character (C0 range and DEL) to the Unicode
/// replacement char `U+FFFD`. Decoded heap Strings can hold arbitrary bytes;
/// without this the Markdown report turns into a "binary file" that corrupts
/// terminals and defeats `grep`/`diff` (HTML/JSON sanitize on their own paths).
pub(crate) fn escape_string_cell(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            '`' => '\'',
            // Remaining C0 controls and DEL become the replacement char.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => '\u{fffd}',
            c => c,
        })
        .collect::<String>()
        .replace('|', "\\|")
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
        heap::ROOT_INTERNED_STRING => Some("Interned String"),
        heap::ROOT_DEBUGGER => Some("Debugger"),
        heap::ROOT_VM_INTERNAL => Some("VM Internal"),
        heap::ROOT_JNI_MONITOR => Some("JNI Monitor"),
        _ => None,
    }
}

// ── Formatting helpers ─────────────────────────────────────────────────────

/// ISO-8601 UTC timestamp matching java.time.Instant.toString() shape.
/// Non-deterministic — parity comparison ignores this line.
pub fn now_iso8601() -> String {
    #[cfg(not(target_family = "wasm"))]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        format_epoch_nanos(now.as_secs(), now.subsec_nanos())
    }
    #[cfg(target_family = "wasm")]
    {
        format_epoch_nanos(0, 0)
    }
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

/// Human-readable byte size (`B`/`KB`/`MB`/`GB`/`TB`/`PB`, binary 1024 base).
/// Used only for display; the JSON model always carries raw `u64` byte counts.
/// All units are powers of 1024 (the report's "sizes are binary" convention);
/// see [`SIZE_BASIS_CAPTION`].
pub fn format_bytes(n: u64) -> String {
    // Pick the unit by MAGNITUDE, then guard the boundary: a value just below a
    // unit threshold (e.g. 1 MiB - 1) would otherwise round up to "1024.0 KB".
    // If the rounded mantissa reaches 1024, promote to the next unit.
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;
    const PB: u64 = 1024 * TB;
    if n < KB {
        return format!("{} B", n);
    }
    let kb = n as f64 / KB as f64;
    if n < MB && (kb * 10.0).round() < 1024.0 * 10.0 {
        return format!("{:.1} KB", kb);
    }
    let mb = n as f64 / MB as f64;
    if n < GB && (mb * 10.0).round() < 1024.0 * 10.0 {
        return format!("{:.1} MB", mb);
    }
    let gb = n as f64 / GB as f64;
    if n < TB && (gb * 100.0).round() < 1024.0 * 100.0 {
        return format!("{:.2} GB", gb);
    }
    let tb = n as f64 / TB as f64;
    if n < PB && (tb * 100.0).round() < 1024.0 * 100.0 {
        return format!("{:.2} TB", tb);
    }
    format!("{:.2} PB", n as f64 / PB as f64)
}

/// One-line caption noting that all byte sizes use binary (1024-based) units.
/// Rendered once near the top of every format so KB/MB/GB/TB/PB are unambiguous.
pub const SIZE_BASIS_CAPTION: &str =
    "_All sizes are binary (1 KB = 1024 bytes, 1 MB = 1024 KB, and so on)._";

/// Group an unsigned integer into comma-separated thousands (e.g. `1234567` ->
/// `1,234,567`). Shared by every count display; the signed delta grouper in
/// `diff_reports` delegates here for identical grouping.
pub(crate) fn fmt_count(n: u64) -> String {
    group_thousands(&n.to_string())
}

/// `"1 object"` vs `"N objects"`. Use with `fmt_count` for the number part.
pub(crate) fn plural_objects(n: u64) -> &'static str {
    if n == 1 { "object" } else { "objects" }
}

/// Insert comma thousands-separators into a bare decimal-digit string. The one
/// place grouping is implemented; `fmt_count` and the diff signed-delta grouper
/// both call it so grouping can never drift between count displays.
pub(crate) fn group_thousands(digits: &str) -> String {
    let mut result = String::new();
    for (i, c) in digits.chars().rev().enumerate() {
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
pub(crate) const DEPTH_DIST_CAPTION: &str = "_How many dominator hops each object sits \
below a GC root. A spike at depth 1–3 is normal; a long tail at depth 10+ points to \
deeply nested containers or linked structures._\n\n";

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
/// A tiny-but-nonzero share (0 < p < 0.05, which would otherwise round to the
/// misleading "0.0%") is shown as "<0.1%" so a reader never sees a nonzero
/// contributor reported as exactly zero (§41.3). Exact zero stays "0.0%".
pub(crate) fn fmt_pct(p: f64) -> String {
    if p > 0.0 && p < 0.05 {
        return "<0.1%".to_string();
    }
    format!("{p:.1}%")
}

/// The single canonical name for the denominator behind every "% Heap" figure in
/// the report: total reachable shallow heap (`SystemOverview.total_shallow`,
/// mirrored on `LeakSuspects.total_shallow`). Every renderer and the triage rules
/// use this label so all four formats agree on what "% Heap" means.
pub const HEAP_BASIS_LABEL: &str = "reachable heap";

/// Canonical label for the total reachable shallow heap scalar — used wherever
/// the raw byte count is displayed so all four formats agree (§48.1).
pub const HEAP_SCALAR_LABEL: &str = "Total Reachable Heap";

/// Share of the reachable heap, as a 0.0–100.0 percentage. `part` is a retained
/// or shallow byte count; `total` is the canonical reachable-shallow total
/// ([`HEAP_BASIS_LABEL`]). Returns `0.0` when `total` is zero and **clamps to
/// 100.0** so a rounding artifact or a retained figure that momentarily exceeds
/// the shallow basis can never print e.g. "104.2% of reachable heap" (§45.1).
pub(crate) fn pct_of_heap(part: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (part as f64 / total as f64 * 100.0).min(100.0)
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
/// Human-readable kind label for object `i`, matching the KIND_ORDER buckets
/// in `build_system_overview`. Retained as the `#[cfg(test)]` oracle for that
/// function's inline `kind_idx_of` (which computes the bucket INDEX directly,
/// without this string, to avoid ~n string round-trips in the fused hot loop).
#[cfg(test)]
pub(crate) fn object_kind(g: &Graph, i: usize) -> &'static str {
    if class_obj_repr(g, i) != u32::MAX {
        return "Class Objects";
    }
    let raw = match g.class_names.get(g.class_idx[i] as usize) {
        Some(r) => r,
        None => return "Instances",
    };
    if is_prim_array_desc(raw) {
        "Primitive Arrays"
    } else if raw.starts_with('[') {
        "Object Arrays"
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
/// Reference package-path renderer, retained as the byte-exact oracle for
/// `package_segments` (production uses `package_segments` to avoid per-dominator
/// allocation). Test-only: the model build no longer calls this.
#[cfg(test)]
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

/// Append the package-path segments of `name` (borrowed, no per-call heap
/// allocation) into `out`, in the exact order `package_path(name).split('.')`
/// would. Lets the Biggest-Packages tree build walk millions of top-level
/// dominators without allocating a `String` per dominator (the
/// `replace('/', ".")` + `to_string()` in `package_path`). The caller reuses one
/// `Vec` across dominators (clear + refill), so only its capacity persists and
/// only the BTreeMap keys that are actually inserted allocate.
///
/// Class names separate packages with `/` (nested classes use `$`, arrays `[`),
/// so the only `.`-producing char in `package_path` is the `/`→`.` replace. The
/// package path is everything before the final `/` (the class-name component is
/// dropped). Primitives/arrays collapse to `(primitives)`; a default-package
/// class (no `/`) becomes `(default)` — matching `package_path` byte-for-byte.
pub(crate) fn package_segments<'a>(name: &'a str, out: &mut Vec<&'a str>) {
    out.clear();
    let mut s = name;
    while let Some(rest) = s.strip_prefix('[') {
        s = rest;
    }
    if let Some(inner) = s.strip_prefix('L') {
        if let Some(inner) = inner.strip_suffix(';') {
            s = inner;
        }
    }
    if s.is_empty()
        || matches!(s, "B" | "C" | "D" | "F" | "I" | "J" | "S" | "Z")
        || s.ends_with("[]")
    {
        out.push("(primitives)");
        return;
    }
    match s.rfind('/') {
        Some(slash) => {
            for seg in s[..slash].split('/') {
                out.push(seg);
            }
        }
        None => out.push("(default)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_units_and_boundaries() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
        // Boundary: just under 1 MiB must not round up to "1024.0 KB".
        assert_eq!(format_bytes(1024 * 1024 - 1), "1.0 MB");
    }

    #[test]
    fn format_bytes_tb_pb() {
        const TB: u64 = 1024u64.pow(4);
        const PB: u64 = 1024u64.pow(5);
        assert_eq!(format_bytes(TB), "1.00 TB");
        assert_eq!(format_bytes(5 * TB + TB / 2), "5.50 TB");
        assert_eq!(format_bytes(PB), "1.00 PB");
        assert_eq!(format_bytes(3 * PB), "3.00 PB");
        // Just under 1 TiB stays in GB, not a premature "1024.00 GB".
        assert_eq!(format_bytes(TB - 1), "1.00 TB");
    }

    #[test]
    fn fmt_pct_tiny_nonzero_is_lt_point_one() {
        assert_eq!(fmt_pct(0.0), "0.0%");
        assert_eq!(fmt_pct(0.01), "<0.1%");
        assert_eq!(fmt_pct(0.049), "<0.1%");
        assert_eq!(fmt_pct(0.05), "0.1%");
        assert_eq!(fmt_pct(12.34), "12.3%");
        assert_eq!(fmt_pct(100.0), "100.0%");
    }

    #[test]
    fn fmt_count_grouping() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_000), "1,000");
        assert_eq!(fmt_count(1_234_567), "1,234,567");
    }

    #[test]
    fn pct_of_heap_clamps_and_guards_zero() {
        assert_eq!(pct_of_heap(0, 0), 0.0);
        assert_eq!(pct_of_heap(50, 100), 50.0);
        // Retained can exceed the shallow basis; the share must clamp at 100.
        assert_eq!(pct_of_heap(150, 100), 100.0);
    }
}
