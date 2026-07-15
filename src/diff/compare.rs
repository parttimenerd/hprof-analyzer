//! Comparison and classification: match a parsed `MatReport` against our
//! `report::Report`, classify each field into MATCH / EXPLAINABLE / FAIL, and
//! drive the `--diff` subcommand. Byte-for-byte identical to the pre-split
//! `diff.rs`.

use std::io;

use super::*;
use crate::report::{self, Report};

// ── Comparison / classification ──────────────────────────────────────────────

/// Prove the `java.lang.Class`-only rooting gap: for every class BOTH tools
/// export, all non-`java.lang.Class` classes match objects+shallow exactly and
/// the ONLY class that differs is `java.lang.Class`. Returns Some(proof-string)
/// if the exemption is granted, None if the proof does not hold (=> FAIL).
pub(crate) fn class_gap_proof(mat: &MatReport, ours: &Report) -> Option<String> {
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
pub(crate) fn classify_int(
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
pub(crate) fn mat_bytes_band(disp: &str) -> Option<(f64, f64)> {
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
pub(crate) fn pct_string(retained: u64, denom: u64) -> String {
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

    // ── Top consumers: Biggest Objects / Classes / Packages ──
    compare_biggest_objects(mat, ours, &mut r);
    compare_biggest_classes(mat, ours, &mut r);
    compare_packages(mat, ours, &mut r);

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
pub(crate) fn compare_histogram(mat: &MatReport, ours: &Report, r: &mut DiffResult) {
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
pub(crate) fn compare_suspects(mat: &MatReport, ours: &Report, r: &mut DiffResult) {
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
                // Retained bytes: EXACT, with the single documented exemption
                // for `java.lang.Class` — MAT roots extra java.lang.Class
                // objects (the metadata-only object-rooting gap), so a
                // java.lang.Class suspect's retained subtree legitimately
                // differs. This mirrors the histogram comparator's treatment of
                // the java.lang.Class row and is name-gated to that one class;
                // it is NOT a numeric tolerance band.
                r.fields
                    .push(classify_int(&field, os.retained, ms.retained, || {
                        if ms.class_name == "java.lang.Class" {
                            Some(Explanation::MatClassObjectRootingGap {
                                proof: "java.lang.Class suspect retained differs by the \
                                    documented object-rooting gap (metadata-only)"
                                    .to_string(),
                            })
                        } else {
                            None
                        }
                    }));
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
                } else if ms.class_name == "java.lang.Class"
                    && pct_string(ms.retained, denom) == mat_pct
                {
                    // The pct diverges ONLY because the java.lang.Class retained
                    // diverges (the object-rooting gap already accepted above):
                    // ours faithfully renders OUR retained and MAT faithfully
                    // renders MAT's larger retained. Proven consistent (MAT's
                    // printed pct == round(MAT_retained/denom)); same root cause,
                    // not a numeric tolerance band.
                    r.fields.push(FieldDiff::explained(
                        pfield,
                        our_pct,
                        mat_pct,
                        Explanation::MatClassObjectRootingGap {
                            proof: "java.lang.Class pct follows the retained object-rooting \
                                    gap; each side renders its own retained faithfully"
                                .to_string(),
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

/// Compare MAT's "Biggest Objects" rows against our `top.biggest_objects`.
/// Each MAT row is matched to one of ours by (normalized class name, shallow,
/// retained) — the (shallow, retained) pair disambiguates legitimately
/// duplicated class names (e.g. several `ZipFile$Source` objects). All values
/// are exact; retained bytes are the same dominator-subtree sum.
pub(crate) fn compare_biggest_objects(mat: &MatReport, ours: &Report, r: &mut DiffResult) {
    if mat.biggest_objects.is_empty() {
        return;
    }
    // Track which of our rows have already been consumed so two identical MAT
    // rows do not both match a single one of ours.
    let mut used = vec![false; ours.top.biggest_objects.len()];
    for (i, mo) in mat.biggest_objects.iter().enumerate() {
        let field = format!("top.biggest_object[{i}:{}]", mo.class_name);
        let mat_s = format!("sh={} ret={}", mo.shallow, mo.retained);
        // Prefer an unused, fully-exact match (name+shallow+retained). The
        // class name is compared with array-length annotations normalized away.
        let exact = ours.top.biggest_objects.iter().enumerate().find(|(j, o)| {
            !used[*j]
                && normalize_array_len(&o.display_class) == mo.class_name
                && o.shallow == mo.shallow
                && o.retained == mo.retained
        });
        if let Some((j, o)) = exact {
            used[j] = true;
            r.fields.push(FieldDiff::matched(
                field,
                format!("sh={} ret={}", o.shallow, o.retained),
                mat_s,
            ));
            continue;
        }
        // No exact match: surface the closest same-name (unused) row for the
        // FAIL detail, else report as missing. Never laundered.
        match ours
            .top
            .biggest_objects
            .iter()
            .enumerate()
            .find(|(j, o)| !used[*j] && normalize_array_len(&o.display_class) == mo.class_name)
        {
            Some((j, o)) => {
                used[j] = true;
                r.fields.push(FieldDiff::failed(
                    field,
                    format!("sh={} ret={}", o.shallow, o.retained),
                    mat_s,
                ));
            }
            None => {
                r.fields.push(FieldDiff::failed(field, "(missing)", mat_s));
            }
        }
    }
}

/// Compare MAT's "Biggest Top-Level Dominator Classes" rows against our
/// `top.biggest_classes`, keyed by class name. Instances + retained are exact.
pub(crate) fn compare_biggest_classes(mat: &MatReport, ours: &Report, r: &mut DiffResult) {
    use std::collections::HashMap;
    if mat.biggest_classes.is_empty() {
        return;
    }
    let our_by_name: HashMap<&str, &report::ClassRow> = ours
        .top
        .biggest_classes
        .iter()
        .map(|c| (c.pretty_class.as_str(), c))
        .collect();
    for mc in &mat.biggest_classes {
        let field = format!("top.biggest_class[{}]", mc.class_name);
        let mat_s = format!("obj={} ret={}", mc.objects, mc.retained);
        match our_by_name.get(mc.class_name.as_str()) {
            None => {
                r.fields.push(FieldDiff::failed(field, "(missing)", mat_s));
            }
            Some(oc) => {
                let ours_s = format!("obj={} ret={}", oc.instances, oc.retained);
                if oc.instances == mc.objects && oc.retained == mc.retained {
                    r.fields.push(FieldDiff::matched(field, ours_s, mat_s));
                } else {
                    r.fields.push(FieldDiff::failed(field, ours_s, mat_s));
                }
            }
        }
    }
}

/// Is this dotted package path on the `java.lang` chain (root, `java`,
/// `java.lang`, or a descendant of `java.lang`)?
fn on_java_lang_path(path: &str) -> bool {
    path.is_empty() || path == "java" || path == "java.lang" || path.starts_with("java.lang.")
}

/// Prove the package-tree retained divergence is the documented
/// `java.lang.Class` object-rooting gap: MAT roots ONE (or more) extra
/// top-level dominator(s), all java.lang-related, that we do not, so the ONLY
/// packages whose retained differs are those on the `java.lang` chain, and each
/// such node also shows MAT's top-dominator count strictly greater than ours
/// (the extra rooted object). Returns Some(proof) iff EVERY divergent package
/// satisfies both conditions; None otherwise (=> divergences FAIL).
pub(crate) fn package_gap_proof(
    mat: &MatReport,
    our_by_path: &std::collections::HashMap<String, &report::PackageNode>,
) -> Option<String> {
    let mut divergent = 0usize;
    for mp in &mat.packages {
        let Some(on) = our_by_path.get(&mp.dotted_path) else {
            continue; // class-leaf, no counterpart — handled as SKIP elsewhere
        };
        if on.retained_heap == mp.retained {
            continue;
        }
        divergent += 1;
        // A divergent package NOT on the java.lang chain voids the proof.
        if !on_java_lang_path(&mp.dotted_path) {
            return None;
        }
        // The divergence must be accompanied by MAT rooting more top-level
        // dominators than us at this node (the extra rooted object). If MAT's
        // count is <= ours yet retained differs, this is not the rooting gap.
        if mp.top_dominators <= on.top_dominator_count {
            return None;
        }
    }
    if divergent == 0 {
        return None;
    }
    Some(format!(
        "package retained delta confined to the java.lang chain ({divergent} node(s)); \
         MAT roots extra top-level dominator(s) there (java.lang.Class object-rooting gap)"
    ))
}

/// Compare MAT's "Biggest Top-Level Dominator Packages" tree against our
/// `top.biggest_packages` (PackageNode tree). Matched by dotted package path;
/// retained bytes are exact. MAT descends one level deeper than we do (into
/// class-name leaves under each package); those class-leaf rows have no
/// PackageNode counterpart and are tier-iv SKIPs. A package present on both
/// sides whose retained differs is a FAIL, EXCEPT the one documented benign
/// case: the `java.lang.Class` object-rooting gap, proven by
/// `package_gap_proof` (divergence confined to the java.lang chain, each such
/// node carrying MAT's extra rooted top-level dominator).
pub(crate) fn compare_packages(mat: &MatReport, ours: &Report, r: &mut DiffResult) {
    use std::collections::HashMap;
    if mat.packages.is_empty() {
        return;
    }
    // Flatten our package tree into a path -> node map (root path = "").
    let mut our_by_path: HashMap<String, &report::PackageNode> = HashMap::new();
    fn walk<'a>(
        node: &'a report::PackageNode,
        path: &str,
        map: &mut HashMap<String, &'a report::PackageNode>,
    ) {
        map.insert(path.to_string(), node);
        for child in &node.children {
            let child_path = if path.is_empty() {
                child.name.clone()
            } else {
                format!("{path}.{}", child.name)
            };
            walk(child, &child_path, map);
        }
    }
    walk(&ours.top.biggest_packages, "", &mut our_by_path);

    // Prove (or refute) the java.lang.Class package-rooting gap once.
    let pkg_gap = package_gap_proof(mat, &our_by_path);

    for mp in &mat.packages {
        let label = if mp.dotted_path.is_empty() {
            "<all>".to_string()
        } else {
            mp.dotted_path.clone()
        };
        let field = format!("top.package[{label}].retained");
        let mat_s = mp.retained.to_string();
        match our_by_path.get(&mp.dotted_path) {
            None => {
                // MAT descends into class-name leaves we do not model as
                // package nodes -> tier-iv skip, not a FAIL.
                r.skipped.push(FieldDiff::explained(
                    field,
                    "(no package-node counterpart)",
                    mat_s,
                    Explanation::NoCounterpart {
                        note: "MAT package tree descends into a class-name leaf we do not model"
                            .to_string(),
                    },
                ));
            }
            Some(on) => {
                let ours_s = on.retained_heap.to_string();
                if on.retained_heap == mp.retained {
                    r.fields.push(FieldDiff::matched(field, ours_s, mat_s));
                } else if on_java_lang_path(&mp.dotted_path) && pkg_gap.is_some() {
                    r.fields.push(FieldDiff::explained(
                        field,
                        ours_s,
                        mat_s,
                        Explanation::MatClassObjectRootingGap {
                            proof: pkg_gap.clone().unwrap(),
                        },
                    ));
                } else {
                    r.fields.push(FieldDiff::failed(field, ours_s, mat_s));
                }
            }
        }
    }
}
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
