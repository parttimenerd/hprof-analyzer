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

mod compare;

mod model;

mod parse;

pub use compare::*;

pub use model::*;

pub use parse::*;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // Tests build `MatReport` incrementally (default then assign named fields
    // for readability); the struct-update alternative is noisier here.
    #![allow(clippy::field_reassign_with_default)]
    use super::*;
    use crate::report::{
        self, HistRow, LeakSuspects, Report, SCHEMA_VERSION, Suspect, SystemOverview, TopConsumers,
    };

    fn hist(name: &str, inst: u64, sh: u64, ret: u64) -> HistRow {
        HistRow {
            pretty_class: name.to_string(),
            instances: inst,
            shallow: sh,
            retained: ret,
            max_instance_shallow: 0,
            loader_id: 0,
            loader_label: None,
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
                unreachable_retained: 0,
                unreachable_composition: Default::default(),
                unreachable_garbage_roots: vec![],
                unreachable_histogram: vec![],
                histogram,
                histogram_truncated_to: None,
                system_properties: vec![],
                jvm_version: None,
                loader_rollup: vec![],
                duplicate_classes: vec![],
                record_census: Default::default(),
                duplicate_strings: None,
                duplicate_prim_arrays: None,
                heap_fragmentation_ratio: 0.0,
                top_class_concentration_bp: 0,
                gc_roots_retained_by_type: vec![],
                boxed_numbers: vec![],
                header_overhead: vec![],
                boxed_number_holders: vec![],
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
                size_distribution: Default::default(),
            },
            threads: crate::report::ThreadOverview { threads: vec![] },
            top_components: crate::report::TopComponents::default(),
            alloc_sites: None,
            arrays_by_size: Default::default(),
            dominator_analysis: Default::default(),
            collections: Default::default(),
            references: Default::default(),
            collection_attribution: None,
            fields_by_size: None,
            biggest_collections: None,
            collection_contents: None,
            leak_indicators: Default::default(),
            triage: Vec::new(),
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
            dominated_total_count: 0,
            dominated_shown: 0,
            dominated_by_class: vec![],
            keywords: vec![],
            root_type_label: String::new(),
            root_path: None,
            dominator_tree: None,
            merged_paths: None,
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

    // ── Top Consumers parsing (Biggest Objects / Classes / Packages) ──

    // parse_top_consumers extracts all three tables and normalizes array-length
    // annotations on object labels; class-loader rows in the classes table are
    // rejected (they share the header but are not classes).
    #[test]
    fn parse_top_consumers_all_three_tables() {
        let html = r####"<html><body>
            <table class="result">
              <thead><tr><th>Class Name</th><th>Shallow Heap</th><th>Retained Heap</th></tr></thead>
              <tbody>
                <tr><td><img><a href="mat://object/0x809002b0">scala.InstanceBlock[7] @ 0x809002b0</a></td><td align="right">8</td><td align="right">2,791,424</td></tr>
                <tr><td><img><a href="mat://object/0x8e720fb0">class java.lang.Object @ 0x8e720fb0</a></td><td align="right">32</td><td align="right">2,500,000</td></tr>
                <tr class="totals"><td>Total: 3 entries</td><td align="right">40</td><td align="right"></td></tr>
              </tbody>
            </table>
            <table class="result">
              <thead><tr><th>Label</th><th>Number of Objects</th><th>Used Heap Size</th><th>Retained Heap Size</th><th>Retained%</th></tr></thead>
              <tbody>
                <tr><td><img><a href="mat://object/0x1">scala.concurrent.stm.ccstm.InTxnImpl</a></td><td align="right">94</td><td align="right">13,536</td><td align="right">2,791,424</td><td align="right">22.90%</td></tr>
                <tr><td><img><a href="mat://object/0x2">&lt;system class loader&gt;</a></td><td align="right">10</td><td align="right">100</td><td align="right">5,000</td><td align="right">0.04%</td></tr>
                <tr><td><img><a href="mat://object/0x3">java.net.URLClassLoader @ 0x80300d20</a></td><td align="right">5</td><td align="right">50</td><td align="right">4,000</td><td align="right">0.03%</td></tr>
              </tbody>
            </table>
            <table class="result">
              <thead><tr><th>Package</th><th>Retained Heap</th><th>Retained%</th><th># Top Dominators</th></tr></thead>
              <tbody>
                <tr><td><img src="x"><ul><li>&lt;all&gt;</li></ul></td><td align="right">12,187,000</td><td align="right">100%</td><td align="right">25</td></tr>
                <tr><td>+<img src="x"><ul><li>java<a href="q">q</a></li></ul></td><td align="right">3,000,000</td><td align="right">24%</td><td align="right">10</td></tr>
                <tr><td>|+<img src="x"><ul><li>lang<a href="q">q</a></li></ul></td><td align="right">2,000,000</td><td align="right">16%</td><td align="right">7</td></tr>
                <tr><td>+<img src="x"><ul><li>scala<a href="q">q</a></li></ul></td><td align="right">1,500,000</td><td align="right">12%</td><td align="right">3</td></tr>
              </tbody>
            </table>
            </body></html>"####;
        let mut rep = MatReport::default();
        parse_top_consumers(html, &mut rep);

        // Biggest Objects: array length stripped; "class " prefix + @ addr cut.
        assert_eq!(rep.biggest_objects.len(), 2);
        assert_eq!(rep.biggest_objects[0].class_name, "scala.InstanceBlock[]");
        assert_eq!(rep.biggest_objects[0].shallow, 8);
        assert_eq!(rep.biggest_objects[0].retained, 2_791_424);
        assert_eq!(rep.biggest_objects[1].class_name, "java.lang.Object");
        assert_eq!(rep.biggest_objects[1].shallow, 32);

        // Biggest Classes: the two class-loader rows are rejected.
        assert_eq!(rep.biggest_classes.len(), 1);
        assert_eq!(
            rep.biggest_classes[0].class_name,
            "scala.concurrent.stm.ccstm.InTxnImpl"
        );
        assert_eq!(rep.biggest_classes[0].objects, 94);
        assert_eq!(rep.biggest_classes[0].retained, 2_791_424);

        // Packages: root -> java -> lang, then back up to java's sibling scala.
        assert_eq!(rep.packages.len(), 4);
        assert_eq!(rep.packages[0].depth, 0);
        assert_eq!(rep.packages[0].dotted_path, ""); // <all> root
        assert_eq!(rep.packages[0].retained, 12_187_000);
        assert_eq!(rep.packages[1].dotted_path, "java");
        assert_eq!(rep.packages[1].top_dominators, 10);
        assert_eq!(rep.packages[2].dotted_path, "java.lang");
        assert_eq!(rep.packages[2].retained, 2_000_000);
        // scala is a sibling of java (depth 1), NOT java.scala — regression for
        // the truncate-off-by-one path bug.
        assert_eq!(rep.packages[3].dotted_path, "scala");
    }

    // Helpers for the top-consumer comparators.
    fn objrow(display: &str, sh: u64, ret: u64) -> report::ObjRow {
        report::ObjRow {
            obj_index_1based: 1,
            display_class: display.to_string(),
            shallow: sh,
            retained: ret,
            pct_bp: 0,
            pct: 0.0,
            owner: None,
        }
    }
    fn classrow(name: &str, inst: u64, ret: u64) -> report::ClassRow {
        report::ClassRow {
            pretty_class: name.to_string(),
            instances: inst,
            retained: ret,
        }
    }
    fn pkg(
        name: &str,
        doms: u64,
        ret: u64,
        children: Vec<report::PackageNode>,
    ) -> report::PackageNode {
        report::PackageNode {
            name: name.to_string(),
            top_dominator_count: doms,
            shallow_heap: 0,
            retained_heap: ret,
            children,
        }
    }

    // compare_biggest_objects: array-length normalization means a MAT
    // `Foo[131072]` matches our class-level `Foo[]` when shallow+retained agree;
    // a genuine byte delta FAILs.
    #[test]
    fn compare_biggest_objects_normalizes_and_fails_on_delta() {
        let mut ours = base_report(vec![]);
        ours.top.biggest_objects = vec![
            objrow("java.lang.Object[]", 32, 2_500_000),
            objrow("byte[]", 24, 1_170_272),
        ];
        let mut mat = MatReport::default();
        mat.biggest_objects = vec![
            MatBiggestObject {
                class_name: "java.lang.Object[]".into(),
                shallow: 32,
                retained: 2_500_000,
            },
            MatBiggestObject {
                class_name: "byte[]".into(),
                shallow: 24,
                retained: 9_999_999, // wrong
            },
        ];
        let mut r = DiffResult::default();
        compare_biggest_objects(&mat, &ours, &mut r);
        assert_eq!(r.fields.iter().filter(|f| f.tier == Tier::Match).count(), 1);
        assert_eq!(r.n_fail(), 1);
    }

    // Two MAT rows with the same normalized name must consume two distinct rows
    // of ours (the `used` guard), not double-match one.
    #[test]
    fn compare_biggest_objects_dedupes_same_name() {
        let mut ours = base_report(vec![]);
        ours.top.biggest_objects = vec![
            objrow("java.util.zip.ZipFile$Source", 40, 700_000),
            objrow("java.util.zip.ZipFile$Source", 40, 653_616),
        ];
        let mut mat = MatReport::default();
        mat.biggest_objects = vec![
            MatBiggestObject {
                class_name: "java.util.zip.ZipFile$Source".into(),
                shallow: 40,
                retained: 700_000,
            },
            MatBiggestObject {
                class_name: "java.util.zip.ZipFile$Source".into(),
                shallow: 40,
                retained: 653_616,
            },
        ];
        let mut r = DiffResult::default();
        compare_biggest_objects(&mat, &ours, &mut r);
        assert_eq!(r.n_fail(), 0);
        assert_eq!(r.fields.iter().filter(|f| f.tier == Tier::Match).count(), 2);
    }

    // compare_biggest_classes keys by name; exact instances+retained MATCH,
    // a value delta FAILs, an absent class FAILs (never laundered).
    #[test]
    fn compare_biggest_classes_exact_and_missing() {
        let mut ours = base_report(vec![]);
        ours.top.biggest_classes = vec![classrow("scala.Sat", 94, 2_791_424)];
        let mut mat = MatReport::default();
        mat.biggest_classes = vec![
            MatBiggestClass {
                class_name: "scala.Sat".into(),
                objects: 94,
                retained: 2_791_424,
            },
            MatBiggestClass {
                class_name: "not.present.Foo".into(),
                objects: 3,
                retained: 100,
            },
        ];
        let mut r = DiffResult::default();
        compare_biggest_classes(&mat, &ours, &mut r);
        assert_eq!(r.fields.iter().filter(|f| f.tier == Tier::Match).count(), 1);
        assert_eq!(r.n_fail(), 1); // the missing class
    }

    // ── package_gap_proof (positive + refutations) ──

    fn our_pkg_map(
        root: &report::PackageNode,
    ) -> std::collections::HashMap<String, &report::PackageNode> {
        use std::collections::HashMap;
        let mut map: HashMap<String, &report::PackageNode> = HashMap::new();
        fn walk<'a>(
            n: &'a report::PackageNode,
            path: &str,
            m: &mut HashMap<String, &'a report::PackageNode>,
        ) {
            m.insert(path.to_string(), n);
            for c in &n.children {
                let cp = if path.is_empty() {
                    c.name.clone()
                } else {
                    format!("{path}.{}", c.name)
                };
                walk(c, &cp, m);
            }
        }
        walk(root, "", &mut map);
        map
    }

    // Positive: retained delta confined to the java.lang chain, and MAT roots
    // strictly more top-level dominators on each divergent node -> Some(proof).
    #[test]
    fn package_gap_proof_positive() {
        let root = pkg(
            "",
            24,
            12_187_000,
            vec![pkg(
                "java",
                9,
                3_000_000,
                vec![pkg("lang", 6, 2_000_000, vec![])],
            )],
        );
        let map = our_pkg_map(&root);
        let mut mat = MatReport::default();
        mat.packages = vec![
            // root: MAT roots +1 dominator, retained a touch higher.
            MatPackageRow {
                depth: 0,
                segment: String::new(),
                dotted_path: "".into(),
                retained: 12_187_072,
                top_dominators: 25,
            },
            MatPackageRow {
                depth: 1,
                segment: "java".into(),
                dotted_path: "java".into(),
                retained: 3_000_072,
                top_dominators: 10,
            },
            MatPackageRow {
                depth: 2,
                segment: "lang".into(),
                dotted_path: "java.lang".into(),
                retained: 2_000_072,
                top_dominators: 7,
            },
        ];
        assert!(package_gap_proof(&mat, &map).is_some());
    }

    // Refutation A: a divergent package OFF the java.lang chain voids the proof.
    #[test]
    fn package_gap_proof_void_off_path() {
        let root = pkg("", 24, 12_187_000, vec![pkg("scala", 3, 1_500_000, vec![])]);
        let map = our_pkg_map(&root);
        let mut mat = MatReport::default();
        mat.packages = vec![
            MatPackageRow {
                depth: 0,
                segment: String::new(),
                dotted_path: "".into(),
                retained: 12_187_000,
                top_dominators: 24,
            },
            // scala diverges — not on java.lang -> void.
            MatPackageRow {
                depth: 1,
                segment: "scala".into(),
                dotted_path: "scala".into(),
                retained: 1_500_500,
                top_dominators: 4,
            },
        ];
        assert!(package_gap_proof(&mat, &map).is_none());
    }

    // Refutation B: on the java.lang chain but MAT's top-dominator count is NOT
    // strictly greater than ours (no extra rooted object) -> void.
    #[test]
    fn package_gap_proof_void_no_extra_dominator() {
        let root = pkg("", 24, 12_187_000, vec![pkg("java", 9, 3_000_000, vec![])]);
        let map = our_pkg_map(&root);
        let mut mat = MatReport::default();
        mat.packages = vec![
            MatPackageRow {
                depth: 0,
                segment: String::new(),
                dotted_path: "".into(),
                retained: 12_187_000,
                top_dominators: 24,
            },
            // java retained diverges but top_dominators == ours (9) -> not the gap.
            MatPackageRow {
                depth: 1,
                segment: "java".into(),
                dotted_path: "java".into(),
                retained: 3_000_500,
                top_dominators: 9,
            },
        ];
        assert!(package_gap_proof(&mat, &map).is_none());
    }

    // compare_packages end-to-end: exact package MATCHes, class-leaf (no
    // counterpart) SKIPs, and the java.lang gap node is EXPLAINABLE.
    #[test]
    fn compare_packages_match_skip_and_gap() {
        let mut ours = base_report(vec![]);
        ours.top.biggest_packages = pkg(
            "",
            24,
            12_187_000,
            vec![
                pkg(
                    "java",
                    9,
                    3_000_000,
                    vec![pkg("lang", 6, 2_000_000, vec![])],
                ),
                pkg("scala", 3, 1_500_000, vec![]),
            ],
        );
        let mut mat = MatReport::default();
        mat.packages = vec![
            // root + java + lang diverge with an extra rooted dominator (the gap)
            MatPackageRow {
                depth: 0,
                segment: String::new(),
                dotted_path: "".into(),
                retained: 12_187_072,
                top_dominators: 25,
            },
            MatPackageRow {
                depth: 1,
                segment: "java".into(),
                dotted_path: "java".into(),
                retained: 3_000_072,
                top_dominators: 10,
            },
            MatPackageRow {
                depth: 2,
                segment: "lang".into(),
                dotted_path: "java.lang".into(),
                retained: 2_000_072,
                top_dominators: 7,
            },
            // scala matches exactly
            MatPackageRow {
                depth: 1,
                segment: "scala".into(),
                dotted_path: "scala".into(),
                retained: 1_500_000,
                top_dominators: 3,
            },
            // a class-leaf MAT descends into that we do not model
            MatPackageRow {
                depth: 2,
                segment: "Object".into(),
                dotted_path: "java.lang.Object".into(),
                retained: 5,
                top_dominators: 1,
            },
        ];
        let mut r = DiffResult::default();
        compare_packages(&mat, &ours, &mut r);
        assert_eq!(r.n_fail(), 0);
        // scala -> MATCH
        assert!(
            r.fields
                .iter()
                .any(|f| f.field.contains("[scala]") && f.tier == Tier::Match)
        );
        // root/java/lang -> EXPLAINABLE(MatClassObjectRootingGap)
        let gap = r
            .fields
            .iter()
            .filter(|f| {
                matches!(
                    f.tier,
                    Tier::Explainable(Explanation::MatClassObjectRootingGap { .. })
                )
            })
            .count();
        assert_eq!(gap, 3);
        // class-leaf -> SKIP (NoCounterpart)
        assert!(
            r.skipped
                .iter()
                .any(|f| f.field.contains("java.lang.Object"))
        );
    }

    // ── Leak-suspect thread-variant parse ──

    // Variant 3: "The thread java.lang.Thread @ 0x... keeps local variables with
    // total size N ..." — the suspect is a bare <strong> (thread name), instance
    // count is implicitly 1, and the class name is the address-stripped label.
    #[test]
    fn parse_leak_suspect_thread_variant() {
        let html = r####"<html><body>
            <div id="exp2"><div class="important"><div><p>The thread <strong>java.lang.Thread @ 0x8e7ddc48  main</strong> keeps local variables with total size <strong>309,608 (2.54%)</strong> bytes. The top consumers of its minimum retained heap are <strong><q>java.lang.Object</q></strong> (131,072 instances).</p></div></div></div>
            </body></html>"####;
        let mut rep = MatReport::default();
        parse_leak_suspects(html, &mut rep);
        assert_eq!(rep.suspects.len(), 1);
        let s = &rep.suspects[0];
        assert_eq!(s.class_name, "java.lang.Thread");
        assert_eq!(s.instance_count, Some(1));
        assert_eq!(s.retained, 309_608);
        assert!((s.pct - 2.54).abs() < 1e-9);
    }

    // ── java.lang.Class suspect exemptions ──

    // A java.lang.Class suspect whose retained (and thus pct) differ from ours is
    // the documented object-rooting gap -> both fields EXPLAINABLE, zero FAIL.
    // A non-Class suspect with the same kind of delta FAILs (name-gated).
    #[test]
    fn suspect_java_lang_class_retained_and_pct_exempt() {
        let mut ours = base_report(vec![]);
        ours.leaks.total_shallow = 12_187_000;
        let mk = |name: &str, ret: u64| Suspect {
            is_single: false,
            pretty_class: name.to_string(),
            instance_count: 1,
            retained: ret,
            shallow: 100,
            path: vec![],
            accumulation_obj_1based: None,
            accumulation_class: None,
            accumulation_retained: None,
            dominated: vec![],
            dominated_total_count: 0,
            dominated_shown: 0,
            dominated_by_class: vec![],
            keywords: vec![],
            root_type_label: String::new(),
            root_path: None,
            dominator_tree: None,
            merged_paths: None,
        };
        // ours: java.lang.Class retained 1,996,000 -> pct 16.38%; MAT roots
        // more -> 2,100,000 -> pct 17.23% (differs in the 2nd decimal, so the
        // pct exemption exercises the MatClassObjectRootingGap branch rather
        // than the Rounding branch).
        ours.leaks.suspects = vec![mk("java.lang.Class", 1_996_000)];
        let mut mat = MatReport::default();
        mat.suspects = vec![MatSuspect {
            class_name: "java.lang.Class".into(),
            instance_count: Some(2793),
            retained: 2_100_000,
            // MAT's printed pct is round(MAT_retained / denom).
            pct: (2_100_000.0 / 12_187_000.0) * 100.0,
        }];
        let mut r = DiffResult::default();
        compare_suspects(&mat, &ours, &mut r);
        assert_eq!(r.n_fail(), 0);
        let ret = r
            .fields
            .iter()
            .find(|f| f.field.ends_with("retained"))
            .unwrap();
        assert!(matches!(
            ret.tier,
            Tier::Explainable(Explanation::MatClassObjectRootingGap { .. })
        ));
        let pct = r.fields.iter().find(|f| f.field.ends_with("pct")).unwrap();
        assert!(matches!(
            pct.tier,
            Tier::Explainable(Explanation::MatClassObjectRootingGap { .. })
        ));

        // The exemption is name-gated: an identical delta on a non-Class suspect
        // FAILs.
        ours.leaks.suspects = vec![mk("scala.Foo", 1_996_000)];
        let mut mat2 = MatReport::default();
        mat2.suspects = vec![MatSuspect {
            class_name: "scala.Foo".into(),
            instance_count: Some(10),
            retained: 2_100_000,
            pct: (2_100_000.0 / 12_187_000.0) * 100.0,
        }];
        let mut r2 = DiffResult::default();
        compare_suspects(&mat2, &ours, &mut r2);
        assert!(r2.n_fail() >= 1);
    }
}
