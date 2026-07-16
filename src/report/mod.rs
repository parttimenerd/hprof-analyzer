//! Report generation: system overview, leak suspects, top consumers.
//!
//! Rendering goes through an explicit, canonical data model: `build_model` reads
//! the `Graph` (including the large per-object arrays) and computes only bounded
//! aggregates into a `Report` (schema_version 1, see `SCHEMA_VERSION`). The
//! renderers then format that same model: plain Markdown (`render_markdown`),
//! Markdown enriched with in-text graphics (`render_markdown_graphs`), and JSON
//! (via serde on `Report`). Keeping every renderer a pure function of the
//! model bounds peak RSS (the model never stores a per-object Vec) and makes
//! ordering deterministic. The default (no-flags) Markdown/JSON output is
//! byte-exact- and golden-tested, so opt-in fields are `Option<T>` +
//! `skip_serializing_if` and absent by default to preserve parity.

mod build;
pub(crate) mod format;
mod model;
mod render_graphs;
mod render_md;

pub use build::*;
pub use format::*;
pub use model::*;
pub use render_graphs::*;
pub use render_md::*;

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md_test::Md;
    use crate::pass2::Graph;
    use std::collections::HashMap;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn test_fmt_count() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(1_000_000), "1,000,000");
        assert_eq!(fmt_count(2_698_510), "2,698,510");
    }

    #[test]
    fn test_pretty_class_name() {
        assert_eq!(pretty_class_name("java/lang/String"), "java.lang.String");
        assert_eq!(pretty_class_name("[I"), "int[]");
        assert_eq!(pretty_class_name("[B"), "byte[]");
        assert_eq!(
            pretty_class_name("[Ljava/lang/String;"),
            "java.lang.String[]"
        );
        assert_eq!(pretty_class_name("[[I"), "int[][]");
        assert_eq!(pretty_class_name("[Z"), "boolean[]");
        assert_eq!(pretty_class_name("[C"), "char[]");
    }

    #[test]
    fn test_package_path() {
        assert_eq!(
            package_path("java/util/concurrent/Foo"),
            "java.util.concurrent"
        );
        assert_eq!(package_path("Foo"), "(default)");
        assert_eq!(package_path("[I"), "(primitives)");
        assert_eq!(package_path("[B"), "(primitives)");
        assert_eq!(package_path("java/lang/String"), "java.lang");
        assert_eq!(package_path("[Ljava/lang/String;"), "java.lang");
        assert_eq!(
            package_path("java/util/concurrent/ConcurrentHashMap$Node"),
            "java.util.concurrent"
        );
    }

    /// Build a tiny synthetic Graph plus the dominator-children CSR that
    /// `build_model` expects, for the objects/classes described below.
    ///
    /// Layout (n objects, vroot = n):
    /// - `idom[i]`  = immediate dominator (u32::MAX = unreachable, n = vroot).
    /// - `class_idx[i]` = class-histogram row for object i.
    /// - `shallow[i]`, `retained[i]` as given.
    /// - `class_names` gives the raw JVM names for each class row.
    /// - `class_obj_class_idx` maps class-object index -> represented class row.
    /// - `has_same[i]` marks objects with a same-class ancestor (excluded from
    ///   class_retained accumulation).
    ///
    /// Returns `(Graph, dc_off, dc_tgt)`.
    #[allow(clippy::too_many_arguments)]
    fn make_graph(
        idom: Vec<u32>,
        class_idx: Vec<u32>,
        shallow: Vec<u32>,
        retained: Vec<u64>,
        class_names: Vec<&str>,
        class_obj: &[(u32, u32)],
        has_same_true: &[usize],
        gc_root_indices: Vec<u32>,
        synthetic_root_count: usize,
    ) -> (Graph, Vec<u32>, Vec<u32>) {
        let n = idom.len();
        let mut class_obj_class_idx: HashMap<u32, u32> = HashMap::new();
        for &(k, v) in class_obj {
            class_obj_class_idx.insert(k, v);
        }
        let mut has_same = crate::bitset::Bitset::with_len(n);
        for &i in has_same_true {
            has_same.set(i);
        }

        // Build dominator-children CSR indexed 0..=n (node n = vroot). Children
        // of node p are all i with idom[i] == p, in ascending object order.
        let mut children: Vec<Vec<u32>> = vec![Vec::new(); n + 1];
        for (i, &d) in idom.iter().enumerate() {
            if d == u32::MAX {
                continue;
            }
            children[d as usize].push(i as u32);
        }
        let mut dc_off: Vec<u32> = Vec::with_capacity(n + 2);
        let mut dc_tgt: Vec<u32> = Vec::new();
        dc_off.push(0);
        for kids in &children {
            dc_tgt.extend_from_slice(kids);
            dc_off.push(dc_tgt.len() as u32);
        }

        let gc_root_types: Vec<u8> = vec![crate::types::heap::ROOT_UNKNOWN; gc_root_indices.len()];
        let g = Graph {
            n,
            format: "JAVA PROFILE 1.0.2".to_string(),
            file_size: 4096,
            source_name: "test.hprof".to_string(),
            // Full path superset of source_name; compressed-oops fixture: 8-byte
            // ids, 4-byte refs. header_timestamp_ms = 1_700_000_000_000
            // (2023-11-14T22:13:20Z).
            file_path: "/tmp/dumps/test.hprof".to_string(),
            id_size: 8,
            ref_size: 4,
            header_timestamp_ms: 1_700_000_000_000,
            gc_root_indices,
            gc_root_types,
            shallow,
            class_idx,
            class_names: class_names.iter().map(|s| s.to_string()).collect(),
            class_loader_id: vec![0u64; class_names.len()],
            loader_labels: std::collections::HashMap::new(),
            thread_stacks: Vec::new(),
            thread_props: std::collections::HashMap::new(),
            thread_local_counts: std::collections::HashMap::new(),
            thread_local_samples: std::collections::HashMap::new(),
            thread_local_frame_samples: std::collections::HashMap::new(),
            system_properties: Vec::new(),
            jvm_version: None,
            class_obj_class_idx,
            fwd_offsets: Vec::new(),
            fwd_targets: Vec::new(),
            synthetic_root_count,
            system_classloader_shallow: None,
            idom,
            retained,
            has_same_class_ancestor: has_same,
            alloc_stack_serial: Vec::new(),
            alloc_frames_by_serial: None,
            record_census: crate::pass2::RecordCensus {
                utf8_records: 111,
                load_class_records: 22,
                unload_class_records: 3,
                stack_frame_records: 44,
                stack_trace_records: 5,
                heap_dump_segments: 1,
                instance_dumps: 66,
                obj_array_dumps: 7,
                prim_array_dumps: 8,
                class_dumps: 9,
                gc_root_tag_counts: vec![
                    (crate::types::heap::ROOT_JNI_GLOBAL, 10),
                    (crate::types::heap::ROOT_THREAD_OBJ, 2),
                ],
            },
            dup_strings: None,
            arrays_by_size: Default::default(),
            collections: crate::report::CollectionsAnalysis::default(),
            references: crate::report::ReferencesAnalysis::default(),
            reference_referent_idx: [Vec::new(), Vec::new(), Vec::new()],
            collection_attribution_raw: None,
            collection_attribution_truncated: false,
            direct_byte_buffer_capacity_sum: 0,
        };
        (g, dc_off, dc_tgt)
    }

    /// A fixture with 4 reachable objects + 1 unreachable, 3 classes.
    /// - obj0: class0 (com/foo/A), top-level, retained 1000, shallow 100
    /// - obj1: class1 (com/foo/B), top-level, retained 1000, shallow 100 (ties obj0)
    /// - obj2: class0 (com/foo/A), dominated by obj0, retained 50, shallow 50, has_same
    /// - obj3: class2 (org/bar/C), top-level, retained 200, shallow 20
    /// - obj4: class1, UNREACHABLE (idom = MAX), shallow 7
    /// - vroot = 5.
    fn fixture() -> (Graph, Vec<u32>, Vec<u32>) {
        make_graph(
            vec![5, 5, 0, 5, u32::MAX],   // idom
            vec![0, 1, 0, 2, 1],          // class_idx
            vec![100, 100, 50, 20, 7],    // shallow
            vec![1000, 1000, 50, 200, 0], // retained
            vec!["com/foo/A", "com/foo/B", "org/bar/C"],
            &[],           // no class objects
            &[2],          // obj2 has same-class ancestor
            vec![0, 1, 3], // gc roots
            0,             // no synthetic roots
        )
    }

    /// Test-only wrapper: derives the B2 `depth_counts` histogram from `g.idom`
    /// (the way the old per-object memo scan did) and calls the real
    /// `build_model`. Production tallies `depth_counts` for free inside
    /// `compute_retained`'s dominator-tree DFS; test graphs are tiny so
    /// recomputing here is irrelevant.
    fn build_model_t(g: &Graph, dc_off: &[u32], dc_tgt: &[u32], cap: usize) -> Report {
        let n = g.n;
        let vroot = n as u32;
        let undef = u32::MAX;
        let mut depth_counts: Vec<u64> = Vec::new();
        for u in 0..n {
            // A node is reachable iff it has a defined idom (roots have idom
            // = vroot). Walk up to vroot counting hops; depth 1 = under vroot.
            let mut cur = u as u32;
            if g.idom[u] == undef {
                continue;
            }
            let mut depth = 0usize;
            while cur != vroot {
                let p = g.idom[cur as usize];
                if p == undef {
                    depth = 0;
                    break;
                }
                depth += 1;
                cur = p;
            }
            if depth == 0 {
                continue;
            }
            if depth > depth_counts.len() {
                depth_counts.resize(depth, 0);
            }
            depth_counts[depth - 1] += 1;
        }
        build_model(
            g,
            dc_off,
            dc_tgt,
            cap,
            &depth_counts,
            &crate::AnalyzeOptions::default(),
            None,
        )
    }

    #[test]
    fn test_build_model_system_overview() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;
        assert_eq!(o.total_objects, 4);
        assert_eq!(o.total_shallow, 100 + 100 + 50 + 20);
        assert_eq!(o.unreachable_count, 1);
        assert_eq!(o.unreachable_shallow, 7);
        assert_eq!(o.gc_roots, 3);
        assert_eq!(o.classes_loaded, 0);

        // Phase D System Overview cheap fields.
        assert_eq!(o.identifier_size_bits, 64); // id_size 8 bytes * 8
        assert_eq!(o.compressed_oops, Some(true)); // ref_size 4 < id_size 8
        assert_eq!(o.dump_creation, Some(1_700_000_000_000));
        assert_eq!(o.file_path, "/tmp/dumps/test.hprof");
        assert_eq!(o.histogram_truncated_to, None);

        // Histogram: class0 retained = obj0(1000) + obj2 excluded (has_same) = 1000
        //            class1 retained = obj1(1000) = 1000
        //            class2 retained = obj3(200) = 200
        // Sort by retained desc, tie-break ascending class index -> class0, class1, class2.
        assert_eq!(o.histogram.len(), 3);
        assert_eq!(o.histogram[0].pretty_class, "com.foo.A");
        assert_eq!(o.histogram[0].retained, 1000);
        assert_eq!(o.histogram[0].instances, 2); // obj0 + obj2
        assert_eq!(o.histogram[0].shallow, 150);
        // Largest single instance = max(obj0=100, obj2=50) = 100, not the 150 total.
        assert_eq!(o.histogram[0].max_instance_shallow, 100);
        assert_eq!(o.histogram[1].pretty_class, "com.foo.B");
        assert_eq!(o.histogram[1].retained, 1000);
        assert_eq!(o.histogram[1].max_instance_shallow, 100); // single obj1 shallow
        assert_eq!(o.histogram[2].pretty_class, "org.bar.C");
        assert_eq!(o.histogram[2].retained, 200);
    }

    /// GC-roots-by-type breakdown: counts each reachable root by its HPROF
    /// sub-tag label, subtracts the synthetic System-Class roots (so the rows
    /// sum to the reported `gc_roots` scalar), and sorts count-desc / label-asc.
    #[test]
    fn test_gc_roots_by_type_breakdown() {
        use crate::types::heap;
        let (mut g, dc_off, dc_tgt) = fixture();
        // fixture() has 3 reachable roots (obj0, obj1, obj3). Give them types:
        // two System Class (one of which is synthetic) + one Thread. With
        // synthetic_root_count = 1, the synthetic System Class root is removed,
        // leaving System Class = 1 and Thread = 1.
        g.gc_root_types = vec![
            heap::ROOT_SYSTEM_CLASS,
            heap::ROOT_SYSTEM_CLASS,
            heap::ROOT_THREAD_OBJ,
        ];
        g.synthetic_root_count = 1;

        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;
        // Scalar: 3 roots - 1 synthetic = 2.
        assert_eq!(o.gc_roots, 2);
        // Rows must sum to the scalar.
        let sum: u64 = o.gc_roots_by_type.iter().map(|r| r.count).sum();
        assert_eq!(sum, o.gc_roots);
        // Sorted count-desc, then label-asc: both have count 1, so "System
        // Class" (S) precedes "Thread" (T) alphabetically.
        assert_eq!(o.gc_roots_by_type.len(), 2);
        assert_eq!(o.gc_roots_by_type[0].root_type, "System Class");
        assert_eq!(o.gc_roots_by_type[0].count, 1);
        assert_eq!(o.gc_roots_by_type[1].root_type, "Thread");
        assert_eq!(o.gc_roots_by_type[1].count, 1);
    }

    /// When every synthetic root fills a label bucket exactly, that bucket must
    /// be dropped (not left at count 0).
    #[test]
    fn test_gc_roots_by_type_drops_emptied_bucket() {
        use crate::types::heap;
        let (mut g, dc_off, dc_tgt) = fixture();
        // 3 roots: 1 System Class (synthetic) + 2 JNI Global. Removing the 1
        // synthetic System Class empties that bucket entirely.
        g.gc_root_types = vec![
            heap::ROOT_SYSTEM_CLASS,
            heap::ROOT_JNI_GLOBAL,
            heap::ROOT_JNI_GLOBAL,
        ];
        g.synthetic_root_count = 1;

        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;
        assert_eq!(o.gc_roots, 2);
        assert_eq!(o.gc_roots_by_type.len(), 1);
        assert_eq!(o.gc_roots_by_type[0].root_type, "JNI Global");
        assert_eq!(o.gc_roots_by_type[0].count, 2);
    }

    /// The HPROF record census carried from pass1 must flow through the graph
    /// into SystemOverview unchanged, and render into the markdown output.
    #[test]
    fn test_record_census_carried_through() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let c = &r.overview.record_census;
        // Matches the census set in make_graph().
        assert_eq!(c.utf8_records, 111);
        assert_eq!(c.load_class_records, 22);
        assert_eq!(c.unload_class_records, 3);
        assert_eq!(c.stack_frame_records, 44);
        assert_eq!(c.stack_trace_records, 5);
        assert_eq!(c.heap_dump_segments, 1);
        assert_eq!(c.instance_dumps, 66);
        assert_eq!(c.obj_array_dumps, 7);
        assert_eq!(c.prim_array_dumps, 8);
        assert_eq!(c.class_dumps, 9);
        assert_eq!(
            c.gc_root_tag_counts,
            vec![
                (crate::types::heap::ROOT_JNI_GLOBAL, 10),
                (crate::types::heap::ROOT_THREAD_OBJ, 2),
            ]
        );
        // Census renders in both markdown flavors.
        let md = render_markdown(&r);
        assert!(md.contains("HPROF Record Census"), "plain md census header");
        assert!(md.contains("Instance dumps"), "plain md census row");
        assert!(md.contains("JNI Global"), "plain md census root tag");
        let md_g = render_markdown_graphs(&r);
        assert!(
            md_g.contains("HPROF Record Census"),
            "graphs md census header"
        );
    }

    /// The duplicate-Strings block renders stats when present, and a "not run"
    /// note when absent — in both cases the section header appears.
    #[test]
    fn test_duplicate_strings_render() {
        // Present: real stats render (using a byte value large enough to force
        // the byte formatter into KB so we can assert on it deterministically).
        let mut out = String::new();
        let d = Some(crate::pass2::DupStrings {
            distinct_values: 42,
            duplicated_values: 7,
            total_string_instances: 100,
            approx_wasted_bytes: 4096,
            top_duplicated: vec![
                crate::pass2::DupStringSample {
                    text: "hello|world".to_string(),
                    count: 9,
                    len: 11,
                    wasted_bytes: 88,
                },
                crate::pass2::DupStringSample {
                    text: "back`tick".to_string(),
                    count: 3,
                    len: 9,
                    wasted_bytes: 18,
                },
            ],
            length_histogram: vec![
                crate::pass2::StrLenBucket {
                    upper_len: 16,
                    count: 30,
                },
                crate::pass2::StrLenBucket {
                    upper_len: 64,
                    count: 12,
                },
            ],
            length_stats: crate::pass2::StrLenStats {
                min: 3,
                max: 40,
                median: 11,
                total: 512,
            },
            top_string_holders: vec![crate::pass2::StringHolder {
                class_name: "com.example.Config".to_string(),
                string_refs: 55,
            }],
            top_by_length: vec![],
            char_array_waste: None,
        });
        render_duplicate_strings(&mut out, &d, false);
        assert!(
            out.contains("Duplicate Strings (approximate)"),
            "present header"
        );
        assert!(out.contains("Total String instances: 100"), "present total");
        assert!(out.contains("Distinct values: 42"), "present distinct");
        assert!(out.contains("Duplicated values: 7"), "present duplicated");
        assert!(
            out.contains("Approx wasted bytes: 4.0 KB"),
            "present wasted bytes formatted"
        );
        assert!(out.contains("Most-Duplicated Values"), "top-N header");
        assert!(
            out.contains("hello\\|world"),
            "value pipe escaped for table cell"
        );
        assert!(out.contains("back'tick"), "backtick replaced in value cell");
        assert!(
            out.contains("String Length Distribution"),
            "length histogram header"
        );
        assert!(
            out.contains("Classes Holding the Most Strings"),
            "holders header"
        );
        assert!(out.contains("com.example.Config"), "holder class name");
        assert!(out.contains("55"), "holder ref count");

        // md-graphs variant adds a sparkline over the length histogram.
        let mut out_graphs = String::new();
        render_duplicate_strings(&mut out_graphs, &d, true);
        assert!(
            out_graphs.contains("String Length Distribution"),
            "graphs length histogram"
        );

        // Absent: the "not run" note appears under the same header.
        let mut out_none = String::new();
        render_duplicate_strings(&mut out_none, &None, false);
        assert!(
            out_none.contains("Duplicate Strings (approximate)"),
            "none header"
        );
        assert!(out_none.contains("not run"), "none note present");
    }

    // ── B5: heap composition by kind ────────────────────────────────────────

    #[test]
    fn test_object_kind_derivation() {
        // A graph with one of each kind: an instance, an object array, a
        // primitive array, and a class object (present in class_obj_class_idx).
        // idom: all top-level under vroot=4.
        let (g, _dc_off, _dc_tgt) = make_graph(
            vec![4, 4, 4, 4], // idom (vroot = 4)
            vec![0, 1, 2, 3], // class_idx
            vec![16, 24, 32, 8],
            vec![16, 24, 32, 8],
            vec!["com/foo/A", "[Ljava/lang/Object;", "[I", "java/lang/Class"],
            &[(3, 0)], // obj3 is a class object representing class0
            &[],
            vec![],
            0,
        );
        assert_eq!(object_kind(&g, 0), "Instances");
        assert_eq!(object_kind(&g, 1), "Object arrays");
        assert_eq!(object_kind(&g, 2), "Primitive arrays");
        assert_eq!(object_kind(&g, 3), "Class objects");
    }

    #[test]
    fn test_heap_composition_fixed_order_skips_empty() {
        // Two instances + one primitive array; NO object arrays, NO class
        // objects. by_kind must list Instances then Primitive arrays only,
        // preserving the fixed kind order and skipping empty buckets.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![3, 3, 3], // idom (vroot = 3)
            vec![0, 0, 1], // class_idx
            vec![16, 16, 40],
            vec![16, 16, 40],
            vec!["com/foo/A", "[I"],
            &[],
            &[],
            vec![],
            0,
        );
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let bk = &r.overview.heap_composition.by_kind;
        assert_eq!(bk.len(), 2);
        assert_eq!(bk[0].kind, "Instances");
        assert_eq!(bk[0].objects, 2);
        assert_eq!(bk[0].shallow_heap, 32);
        assert_eq!(bk[1].kind, "Primitive arrays");
        assert_eq!(bk[1].objects, 1);
        assert_eq!(bk[1].shallow_heap, 40);
    }

    // ── B2: dominator-depth histogram ───────────────────────────────────────

    #[test]
    fn test_dominator_depth_histogram() {
        // fixture(): obj0/obj1/obj3 are top-level (depth 1); obj2 is dominated
        // by obj0 (depth 2); obj4 is unreachable (excluded).
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let h = &r.overview.dominator_depth_histogram;
        assert_eq!(h.len(), 2);
        // Sorted by depth ascending.
        assert_eq!(h[0].depth, 1);
        assert_eq!(h[0].objects, 3);
        assert_eq!(h[1].depth, 2);
        assert_eq!(h[1].objects, 1);
    }

    // ── B3: retention concentration ─────────────────────────────────────────

    #[test]
    fn test_retention_concentration() {
        // fixture(): top-level dominators retained = [1000, 1000, 200];
        // total_shallow = 270 (denominator). one_pct = 270/100 = 2, so all
        // three top-level objects (>=2) count toward num_objects_ge_1pct.
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let rc = &r.overview.retention_concentration;
        assert_eq!(rc.total_retained, 2200);
        // top1 = 1000/270 = 37037 bp; top10 (all 3) = 2200/270 = 81481 bp.
        assert_eq!(rc.top1_bp, (1000u128 * 10_000 / 270) as u32);
        assert_eq!(rc.top10_bp, (2200u128 * 10_000 / 270) as u32);
        assert_eq!(rc.top100_bp, rc.top10_bp);
        assert_eq!(rc.num_objects_ge_1pct, 3);
    }

    // ── OOM Triage render lines (B2/B3/B5 surfaced) ─────────────────────────

    #[test]
    fn test_render_includes_oom_triage_signals() {
        // Mixed-kind graph so the Heap Composition table renders (>1 kind), with
        // top-level dominators so Shape + One-leak-or-many lines emit.
        // obj0 instance (top-level), obj1 primitive array (top-level),
        // obj2 instance dominated by obj0.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![3, 3, 0], // idom (vroot = 3); obj2 under obj0
            vec![0, 1, 0], // class_idx
            vec![100, 40, 20],
            vec![120, 40, 20],
            vec!["com/foo/A", "[I"],
            &[],
            &[2], // obj2 has same-class ancestor (obj0)
            vec![0, 1],
            0,
        );
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let md = render_markdown(&r);
        assert!(
            md.contains("### Heap Composition"),
            "missing heap composition table"
        );
        assert!(md.contains("**Shape:**"), "missing shape line");
        assert!(
            md.contains("**One leak or many:**"),
            "missing concentration line"
        );
    }

    /// Regression: MAT counts the class histogram BY OBJECT TYPE, so every
    /// `java/lang/Class`-typed object must land in the single `java/lang/Class`
    /// row — including primitive-type Class mirrors (`int.class`, `void.class`,
    /// …) that HPROF stores as plain instances in a *separate* histogram row
    /// that is also named `java/lang/Class`. This test builds both a real class
    /// object and such a mirror instance and asserts they are counted together,
    /// while `classes_loaded` (distinct CLASS_DUMP objects) stays unchanged and
    /// an unrelated class is not miscounted.
    #[test]
    fn test_histogram_folds_duplicate_java_lang_class_rows() {
        // Rows: 0 = java/lang/Class (canonical, used by the class object),
        //       1 = com/foo/A (a normal class),
        //       2 = java/lang/Class (duplicate row: the primitive mirror lands
        //           here because it is a plain instance keyed by the
        //           java/lang/Class class-object address).
        // Objects:
        //   obj0: class_idx 0, IS a class object (represents row 1), top-level.
        //   obj1: class_idx 1, normal instance of com/foo/A, top-level.
        //   obj2: class_idx 2, java/lang/Class-typed mirror, NOT a class object.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![3, 3, 3],        // idom (vroot = 3)
            vec![0, 1, 2],        // class_idx
            vec![100, 50, 20],    // shallow
            vec![1000, 500, 200], // retained
            vec!["java/lang/Class", "com/foo/A", "java/lang/Class"],
            &[(0, 1)], // obj0 is a class object representing row 1
            &[],       // none excluded from retained accumulation
            vec![0, 1, 2],
            0,
        );
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;

        // classes_loaded counts distinct CLASS_DUMP objects (class_obj_repr set)
        // — only obj0. The fold must NOT change this.
        assert_eq!(o.classes_loaded, 1);
        assert_eq!(o.total_objects, 3);

        // Exactly ONE java.lang.Class histogram row, counting BOTH the class
        // object (obj0) and the primitive mirror (obj2).
        let jlc_rows: Vec<&HistRow> = o
            .histogram
            .iter()
            .filter(|h| h.pretty_class == "java.lang.Class")
            .collect();
        assert_eq!(
            jlc_rows.len(),
            1,
            "duplicate java.lang.Class rows not folded"
        );
        assert_eq!(
            jlc_rows[0].instances, 2,
            "mirror not counted under java.lang.Class"
        );
        // Shallow of both mirror + class object moved into the folded row.
        assert_eq!(jlc_rows[0].shallow, 120);

        // The unrelated class is not miscounted.
        let a_row = o
            .histogram
            .iter()
            .find(|h| h.pretty_class == "com.foo.A")
            .expect("com.foo.A row present");
        assert_eq!(a_row.instances, 1);

        // Biggest Classes (over top-level dominators) also folds by type.
        let jlc_big: Vec<&ClassRow> = r
            .top
            .biggest_classes
            .iter()
            .filter(|c| c.pretty_class == "java.lang.Class")
            .collect();
        assert_eq!(jlc_big.len(), 1);
        assert_eq!(jlc_big[0].instances, 2);
    }

    /// Task 19: class-loader identity flows Graph -> report. Three classes, two
    /// distinct loaders (0 = boot, 0x1000). Two of the three are reachable class
    /// objects (mapped via class_obj_class_idx). `classloaders_loaded` counts
    /// distinct loaders among reachable class objects; each HistRow carries the
    /// loader of its class; the Markdown renders a "Class loaders" line.
    #[test]
    fn test_class_loader_plumbing() {
        // Rows: 0 = com/foo/A (loader 0x1000), 1 = com/foo/B (loader 0x1000),
        //       2 = org/bar/C (loader 0 = boot).
        // Objects: obj0 IS a class object -> row 0; obj1 IS a class object ->
        // row 2; obj2 is a plain instance of row 1. vroot = 3.
        let (mut g, _dc_off, _dc_tgt) = make_graph(
            vec![3, 3, 3],        // idom (vroot = 3)
            vec![0, 2, 1],        // class_idx
            vec![100, 50, 20],    // shallow
            vec![1000, 500, 200], // retained
            vec!["com/foo/A", "com/foo/B", "org/bar/C"],
            &[(0, 0), (1, 2)], // obj0 -> row 0, obj1 -> row 2 (class objects)
            &[],
            vec![0, 1, 2],
            0,
        );
        // Assign loaders per histogram row: rows 0,1 = 0x1000; row 2 = boot(0).
        g.class_loader_id = vec![0x1000, 0x1000, 0];
        let (dc_off, dc_tgt) = crate::retained::build_dom_children_csr(g.n, &g.idom);
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;

        // Reachable class objects: obj0 (row 0, loader 0x1000) and obj1 (row 2,
        // loader 0). Two distinct loaders.
        assert_eq!(o.classes_loaded, 2);
        assert_eq!(o.classloaders_loaded, 2);

        // Each HistRow carries its class's loader.
        let a = o
            .histogram
            .iter()
            .find(|h| h.pretty_class == "com.foo.A")
            .expect("com.foo.A row");
        assert_eq!(a.loader_id, 0x1000);
        let c = o
            .histogram
            .iter()
            .find(|h| h.pretty_class == "org.bar.C")
            .expect("org.bar.C row");
        assert_eq!(c.loader_id, 0);

        // Markdown surfaces the Class loaders line.
        let md = render_markdown(&r);
        assert!(md.contains("Class loaders"), "missing Class loaders line");
    }

    /// A boot-only heap (all loaders 0) reports exactly one class loader.
    #[test]
    fn test_class_loader_boot_only() {
        let (mut g, _dc_off, _dc_tgt) = make_graph(
            vec![2, 2],
            vec![0, 1],
            vec![100, 50],
            vec![1000, 500],
            vec!["java/lang/Class", "com/foo/A"],
            &[(0, 1)], // obj0 is a class object representing row 1
            &[],
            vec![0, 1],
            0,
        );
        g.class_loader_id = vec![0, 0];
        let (dc_off, dc_tgt) = crate::retained::build_dom_children_csr(g.n, &g.idom);
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        assert_eq!(r.overview.classloaders_loaded, 1);
    }

    /// Stage 1: `HistRow.loader_label` resolves the boot loader (addr 0) to
    /// `<boot>` and a named loader address to its label from `loader_labels`.
    /// The Markdown "Class loaders (labels)" line lists the non-boot label.
    #[test]
    fn test_loader_label_resolution() {
        // Row 0 = boot-loaded (addr 0); row 1 = loaded by 0x1234.
        let (mut g, _dc_off, _dc_tgt) = make_graph(
            vec![2, 2],
            vec![0, 1],
            vec![100, 50],
            vec![1000, 500],
            vec!["java/lang/Class", "com/foo/A"],
            &[(0, 0), (1, 1)],
            &[],
            vec![0, 1],
            0,
        );
        g.class_loader_id = vec![0, 0x1234];
        g.loader_labels
            .insert(0x1234, "com/example/MyLoader".to_string());
        let (dc_off, dc_tgt) = crate::retained::build_dom_children_csr(g.n, &g.idom);
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;

        let boot = o
            .histogram
            .iter()
            .find(|h| h.loader_id == 0)
            .expect("boot-loaded row present");
        assert_eq!(boot.loader_label.as_deref(), Some("<boot>"));

        let named = o
            .histogram
            .iter()
            .find(|h| h.loader_id == 0x1234)
            .expect("0x1234-loaded row present");
        assert_eq!(named.loader_label.as_deref(), Some("com/example/MyLoader"));

        // Markdown surfaces the label (not the boot pseudo-label) in the list.
        let md = render_markdown(&r);
        assert!(
            md.contains("**Class loaders (labels):** com/example/MyLoader"),
            "missing Class loaders labels line; got:\n{md}"
        );
    }

    /// MAT materializes a synthetic <system class loader> object at 0x0 of
    /// class java/lang/ClassLoader (no HPROF record). When
    /// `system_classloader_shallow` is set, the report injects one such object:
    /// +1 total_objects, +sz total_shallow, +1 instance / +sz shallow on the
    /// java.lang.ClassLoader histogram row. With `None`, everything is
    /// unchanged (regression guard). gc_roots/classes_loaded stay untouched.
    #[test]
    fn test_synthetic_system_classloader_injection() {
        // obj0: java/lang/ClassLoader instance, top-level, shallow 72.
        // obj1: com/foo/A instance, top-level.
        let build = || {
            make_graph(
                vec![2, 2],   // idom (vroot = 2)
                vec![0, 1],   // class_idx
                vec![72, 40], // shallow
                vec![72, 40], // retained
                vec!["java/lang/ClassLoader", "com/foo/A"],
                &[], // no class objects
                &[], // none excluded
                vec![0, 1],
                0,
            )
        };

        // None path: nothing injected.
        {
            let (g, dc_off, dc_tgt) = build();
            assert_eq!(g.system_classloader_shallow, None);
            let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
            let o = &r.overview;
            assert_eq!(o.total_objects, 2);
            assert_eq!(o.total_shallow, 72 + 40);
            let cl_row = o
                .histogram
                .iter()
                .find(|h| h.pretty_class == "java.lang.ClassLoader")
                .expect("ClassLoader row present");
            assert_eq!(cl_row.instances, 1);
            assert_eq!(cl_row.shallow, 72);
        }

        // Some(72) path: one synthetic object injected.
        {
            let (mut g, dc_off, dc_tgt) = build();
            g.system_classloader_shallow = Some(72);
            let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
            let o = &r.overview;
            assert_eq!(o.total_objects, 3, "synthetic object not counted");
            assert_eq!(o.total_shallow, 72 + 40 + 72, "synthetic shallow missing");
            assert_eq!(o.gc_roots, 2, "gc_roots must be unchanged");
            assert_eq!(o.classes_loaded, 0, "classes_loaded must be unchanged");
            let cl_row = o
                .histogram
                .iter()
                .find(|h| h.pretty_class == "java.lang.ClassLoader")
                .expect("ClassLoader row present");
            assert_eq!(cl_row.instances, 2, "synthetic instance not in row");
            assert_eq!(cl_row.shallow, 72 + 72, "synthetic shallow not in row");
        }
    }

    #[test]
    fn test_format_epoch_ms_edges() {
        // Negative (pre-1970) inputs clamp to the epoch, identical to ms == 0.
        assert_eq!(format_epoch_ms(-1), format_epoch_ms(0));
        assert_eq!(format_epoch_ms(0), "1970-01-01T00:00:00Z");
        // A known non-zero instant renders the expected second-granularity ISO.
        assert_eq!(format_epoch_ms(1_700_000_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn test_system_overview_uncompressed_and_no_timestamp() {
        // Same fixture, but override the two header-derived fields to cover the
        // OTHER branches: ref_size == id_size (no compressed oops) and a zero
        // header timestamp (no dump-creation instant).
        let (mut g, dc_off, dc_tgt) = fixture();
        g.ref_size = g.id_size; // 8 == 8 -> not compressed
        g.header_timestamp_ms = 0; // no creation timestamp
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let o = &r.overview;
        assert_eq!(o.compressed_oops, Some(false)); // ref_size == id_size
        assert_eq!(o.dump_creation, None); // header_timestamp_ms == 0
    }

    #[test]
    fn test_build_model_top_consumers_package_determinism() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let t = &r.top;

        // Biggest objects: top-level are obj0(1000), obj1(1000), obj3(200).
        // Tie between obj0/obj1 broken by ascending index -> obj0 (index 1), obj1 (index 2).
        assert_eq!(t.biggest_objects.len(), 3);
        assert_eq!(t.biggest_objects[0].obj_index_1based, 1);
        assert_eq!(t.biggest_objects[1].obj_index_1based, 2);
        assert_eq!(t.biggest_objects[2].obj_index_1based, 4);

        // Biggest classes (over top-level only): class0=1000, class1=1000, class2=200.
        assert_eq!(t.biggest_classes[0].pretty_class, "com.foo.A");
        assert_eq!(t.biggest_classes[1].pretty_class, "com.foo.B");
        assert_eq!(t.biggest_classes[2].pretty_class, "org.bar.C");

        // Biggest packages: tree over full dotted paths.
        //   obj0 com/foo/A -> path com.foo (retained 1000, shallow 100)
        //   obj1 com/foo/B -> path com.foo (retained 1000, shallow 100)
        //   obj3 org/bar/C -> path org.bar (retained 200, shallow 20)
        // Root cumulative: retained 2200, shallow 220, count 3.
        let root = &t.biggest_packages;
        assert_eq!(root.name, "");
        assert_eq!(root.retained_heap, 2200);
        assert_eq!(root.shallow_heap, 220);
        assert_eq!(root.top_dominator_count, 3);
        // Children sorted retained-desc then name-asc: "com" (2000) before "org" (200).
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].name, "com");
        assert_eq!(root.children[0].retained_heap, 2000);
        assert_eq!(root.children[0].shallow_heap, 200);
        assert_eq!(root.children[0].top_dominator_count, 2);
        assert_eq!(root.children[1].name, "org");
        assert_eq!(root.children[1].retained_heap, 200);
        // Nested path com -> foo carries the cumulative totals of its subtree.
        assert_eq!(root.children[0].children.len(), 1);
        let foo = &root.children[0].children[0];
        assert_eq!(foo.name, "foo");
        assert_eq!(foo.retained_heap, 2000);
        assert_eq!(foo.shallow_heap, 200);
        assert_eq!(foo.top_dominator_count, 2);
        assert!(foo.children.is_empty());
        // threshold_bp is the MAT 1%-of-total marker.
        assert_eq!(t.threshold_bp, 100);
    }

    #[test]
    fn test_build_model_packages_pruning() {
        // Two top-level dominators: a big one (>=1% of total) and a tiny one
        // (<1% of total). The tiny package's whole subtree must be pruned.
        // big: retained 10000 in com/big/Foo; small: retained 1 in org/tiny/Bar.
        // total = 10001; 1% threshold => keep >= 100.06 (i.e. >= floor via bp math).
        let (g, dc_off, dc_tgt) = make_graph(
            vec![2, 2],     // idom: obj0,obj1 both under vroot (node 2)
            vec![0, 1],     // class_idx
            vec![50, 5],    // shallow
            vec![10000, 1], // retained
            vec!["com/big/Foo", "org/tiny/Bar"],
            &[],
            &[],
            vec![0, 1],
            0,
        );
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let root = &r.top.biggest_packages;
        // Root keeps cumulative totals over ALL dominators (before pruning).
        assert_eq!(root.retained_heap, 10001);
        assert_eq!(root.top_dominator_count, 2);
        // Only the big package survives pruning.
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "com");
        assert_eq!(root.children[0].retained_heap, 10000);
    }

    #[test]
    fn test_build_model_packages_nothing_over_threshold() {
        // Many equal tiny packages: each is well under 1% of the total, so the
        // root ends up with NO children ("nothing over threshold" case).
        // 200 dominators, each retained 1, in packages pkgN/Foo (all distinct).
        let count = 200usize;
        let idom = vec![count as u32; count]; // all top-level (vroot = node `count`)
        let class_idx: Vec<u32> = (0..count as u32).collect();
        let shallow: Vec<u32> = vec![1; count];
        let retained: Vec<u64> = vec![1; count];
        let names: Vec<String> = (0..count).map(|i| format!("pkg{i}/Foo")).collect();
        let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let gc_roots: Vec<u32> = (0..count as u32).collect();
        let (g, dc_off, dc_tgt) = make_graph(
            idom,
            class_idx,
            shallow,
            retained,
            name_refs,
            &[],
            &[],
            gc_roots,
            0,
        );
        let mut r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let root = &r.top.biggest_packages;
        assert_eq!(root.top_dominator_count, count as u64);
        assert!(
            root.children.is_empty(),
            "no single package should exceed 1% of the total"
        );
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);
        let doc = Md::parse(&md);
        let pkgs = doc
            .section("Biggest Packages by Retained Heap")
            .expect("Biggest Packages section present");
        assert!(
            pkgs.body_contains("_No package retains more than 1% of the total retained heap._"),
            "nothing-over-threshold marker must be rendered under Biggest Packages"
        );
        // And the table must have no data rows in this case.
        assert!(
            pkgs.table(0).map(|t| t.rows().is_empty()).unwrap_or(true),
            "no package rows when nothing exceeds the threshold"
        );
    }

    #[test]
    fn test_build_model_leak_suspects() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let l = &r.leaks;
        // total_shallow = 270, threshold = 27. Singles directly under vroot with
        // retained >= 27: obj0(1000), obj1(1000), obj3(200) all qualify.
        assert_eq!(l.total_shallow, 270);
        assert_eq!(l.suspects.len(), 3);
        // Sorted retained desc, ties by class_idx then obj_idx: obj0(class0),
        // obj1(class1) both 1000 -> class0 first; then obj3.
        assert!(l.suspects[0].is_single);
        assert_eq!(l.suspects[0].pretty_class, "com.foo.A");
        assert_eq!(l.suspects[1].pretty_class, "com.foo.B");
        assert_eq!(l.suspects[2].pretty_class, "org.bar.C");
        // Single suspect must have an accumulation path starting at itself.
        assert!(!l.suspects[0].path.is_empty());
        assert_eq!(l.suspects[0].path[0].depth, 0);
        assert_eq!(l.suspects[0].path[0].obj_index_1based, 1);
    }

    #[test]
    fn test_accumulation_point_big_drop_and_leaf() {
        // Two top-level singles under vroot (node 6):
        //   A(obj0) -> B(obj1) -> {C(obj2), D(obj3)}   [big-drop chain]
        //   E(obj4) -> F(obj5)                          [leaf chain]
        // retained: A=1000 B=950 C=500 D=100 E=800 F=700.
        // A->B: 950 >= 1000*0.7=700 -> descend. B's largest child C=500 <
        //   950*0.7=665 -> BIG DROP -> accumulation point is B (the parent).
        // E->F: 700 >= 800*0.7=560 -> descend. F is a leaf -> accumulation is F.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![6, 0, 1, 1, 6, 4],
            vec![0, 1, 2, 3, 4, 5],
            vec![10, 10, 10, 10, 10, 10],
            vec![1000, 950, 500, 100, 800, 700],
            vec!["A", "B", "C", "D", "E", "F"],
            &[],
            &[],
            vec![0, 4],
            0,
        );
        let l = build_leak_suspects(&g, &dc_off, &dc_tgt, DOMINATED_CAP, 30, 5000, 20);
        // Two singles: A (1000) then E (800), retained-desc.
        assert_eq!(l.suspects.len(), 2);
        let a = &l.suspects[0];
        assert_eq!(a.pretty_class, "A");
        // A descends to B and stops (big drop at C): path = [A, B].
        assert_eq!(a.path.len(), 2);
        assert_eq!(a.accumulation_obj_1based, Some(2)); // B is obj1 -> 1-based 2
        assert_eq!(a.accumulation_class, Some("B".to_string()));
        assert_eq!(a.accumulation_retained, Some(950));
        // B's immediately-dominated children, retained-desc: C(500), D(100).
        assert_eq!(a.dominated.len(), 2);
        assert_eq!(a.dominated[0].obj_index_1based, 3); // C = obj2
        assert_eq!(a.dominated[0].retained, 500);
        assert_eq!(a.dominated[1].obj_index_1based, 4); // D = obj3
        assert_eq!(a.dominated[1].retained, 100);
        // Keywords: suspect class + accumulation class.
        assert_eq!(a.keywords, vec!["A".to_string(), "B".to_string()]);

        // E chain: E -> F (leaf) -> accumulation point is F (obj5 -> 1-based 6).
        let e = &l.suspects[1];
        assert_eq!(e.pretty_class, "E");
        assert_eq!(e.accumulation_obj_1based, Some(6));
        assert_eq!(e.accumulation_class, Some("F".to_string()));
        // F is a leaf: no dominated children.
        assert!(e.dominated.is_empty());
    }

    #[test]
    fn test_accumulation_dominated_cap_truncates() {
        // A(obj0) is the accumulation point (its largest child drops below 0.7),
        // with 3 immediately-dominated children B,C,D.
        // retained: A=1000 B=100 C=90 D=80. 100 < 1000*0.7 -> A is accumulation.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![4, 0, 0, 0],
            vec![0, 1, 2, 3],
            vec![10, 10, 10, 10],
            vec![1000, 100, 90, 80],
            vec!["A", "B", "C", "D"],
            &[],
            &[],
            vec![0],
            0,
        );
        // cap = 1 -> only the largest dominated child is listed.
        let l1 = build_leak_suspects(&g, &dc_off, &dc_tgt, 1, 30, 5000, 20);
        assert_eq!(l1.suspects.len(), 1);
        assert_eq!(l1.suspects[0].accumulation_obj_1based, Some(1)); // A itself
        assert_eq!(l1.suspects[0].dominated.len(), 1);
        assert_eq!(l1.suspects[0].dominated[0].obj_index_1based, 2); // B, largest
                                                                     // Default cap -> all three children listed.
        let l2 = build_leak_suspects(&g, &dc_off, &dc_tgt, DOMINATED_CAP, 30, 5000, 20);
        assert_eq!(l2.suspects[0].dominated.len(), 3);
    }

    #[test]
    fn test_leak_suspect_root_type_label() {
        // Fixture GC roots are objects 0, 1, 3 (all single suspects). Override
        // their root types: obj0 -> Thread, obj1 -> UNKNOWN (no label), obj3 ->
        // JNI Global. Suspects sort com.foo.A (obj0), com.foo.B (obj1),
        // org.bar.C (obj3).
        let (mut g, dc_off, dc_tgt) = fixture();
        use crate::types::heap;
        // gc_root_indices is [0, 1, 3]; align types 1:1.
        assert_eq!(g.gc_root_indices, vec![0, 1, 3]);
        g.gc_root_types = vec![
            heap::ROOT_THREAD_OBJ,
            heap::ROOT_UNKNOWN,
            heap::ROOT_JNI_GLOBAL,
        ];

        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let l = &r.leaks;
        // obj0 is a Thread root -> "Thread".
        assert_eq!(l.suspects[0].pretty_class, "com.foo.A");
        assert_eq!(l.suspects[0].root_type_label, "Thread");
        // obj1 is a root but ROOT_UNKNOWN -> no identifiable label (empty).
        assert_eq!(l.suspects[1].pretty_class, "com.foo.B");
        assert_eq!(l.suspects[1].root_type_label, "");
        // obj3 is a JNI Global root -> "JNI Global".
        assert_eq!(l.suspects[2].pretty_class, "org.bar.C");
        assert_eq!(l.suspects[2].root_type_label, "JNI Global");

        // The known labels render as the additive clause; the unknown one does not.
        let mut r2 = r.clone();
        r2.generated = "FIXED".to_string();
        let md = render_markdown(&r2);
        assert!(md.contains("Held by a **Thread** GC root."));
        assert!(md.contains("Held by a **JNI Global** GC root."));
    }

    #[test]
    fn test_leak_suspect_root_type_label_absent_when_not_root() {
        // A single suspect whose object is NOT a GC root gets no label. obj0 is
        // a top-level dominator (single suspect) but we make ONLY obj1 a root,
        // so obj0's suspect has an empty root_type_label.
        let (mut g, dc_off, dc_tgt) = fixture();
        use crate::types::heap;
        g.gc_root_indices = vec![1];
        g.gc_root_types = vec![heap::ROOT_THREAD_OBJ];

        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let l = &r.leaks;
        // obj0 (com.foo.A) is a single suspect but not itself a root -> empty.
        assert_eq!(l.suspects[0].pretty_class, "com.foo.A");
        assert!(l.suspects[0].is_single);
        assert_eq!(l.suspects[0].root_type_label, "");
        // obj1 (com.foo.B) is the Thread root -> labelled.
        assert_eq!(l.suspects[1].pretty_class, "com.foo.B");
        assert_eq!(l.suspects[1].root_type_label, "Thread");
    }

    #[test]
    fn test_leak_suspect_class_object_shows_represented_class() {
        // A single suspect whose object is itself a java.lang.Class MIRROR must
        // print the REPRESENTED class (e.g. scala.runtime.LazyVals$), not
        // "java.lang.Class" (MAT parity). Regression guard for report.rs:1127.
        //
        // 3 objects, 2 class rows:
        //   row0 = java/lang/Class, row1 = scala/runtime/LazyVals$
        //   obj0: class_idx row0 (a Class mirror), registered in
        //         class_obj_class_idx -> represents row1. Top-level, big retained.
        //   obj1: class_idx row1 (a normal instance), dominated by obj0.
        //   vroot = 2.
        let (g, dc_off, dc_tgt) = make_graph(
            vec![2, 0],        // idom: obj0 top-level, obj1 under obj0
            vec![0, 1],        // class_idx
            vec![24, 16],      // shallow
            vec![100_000, 16], // retained
            vec!["java/lang/Class", "scala/runtime/LazyVals$"],
            &[(0, 1)], // obj0 is a class-mirror representing row1
            &[],
            vec![0], // obj0 is a GC root
            0,
        );
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let s = &r.leaks.suspects[0];
        assert!(s.is_single);
        // The represented class, NOT "java.lang.Class".
        assert_eq!(s.pretty_class, "scala.runtime.LazyVals$");
        assert!(s.keywords.contains(&"scala.runtime.LazyVals$".to_string()));
        assert!(!s.keywords.contains(&"java.lang.Class".to_string()));

        let mut r2 = r.clone();
        r2.generated = "FIXED".to_string();
        let md = render_markdown(&r2);
        assert!(md.contains("scala.runtime.LazyVals$"));
    }

    #[test]
    fn test_render_markdown_deterministic() {
        // Build the model twice and assert render output is byte-identical.
        // This specifically guards the Biggest-Packages HashMap sort fix.
        let (g1, off1, tgt1) = fixture();
        let (g2, off2, tgt2) = fixture();
        let mut r1 = build_model_t(&g1, &off1, &tgt1, DOMINATED_CAP);
        let mut r2 = build_model_t(&g2, &off2, &tgt2, DOMINATED_CAP);
        // Neutralise the nondeterministic timestamp line.
        r1.generated = "FIXED".to_string();
        r2.generated = "FIXED".to_string();
        assert_eq!(render_markdown(&r1), render_markdown(&r2));
    }

    #[test]
    fn test_render_markdown_structure() {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);
        assert!(md.starts_with("# Heap Dump Analysis: `test.hprof`\n\n"));
        let doc = Md::parse(&md);
        // Top-level document title is an H1.
        assert_eq!(
            doc.heading("Heap Dump Analysis").map(|h| h.level()),
            Some(1)
        );
        // Major sections are H2.
        assert_eq!(doc.heading("System Overview").map(|h| h.level()), Some(2));
        assert_eq!(doc.heading("Leak Suspects").map(|h| h.level()), Some(2));
        assert_eq!(doc.heading("Top Consumers").map(|h| h.level()), Some(2));
        // Sub-sections are H3, nested under their parents.
        assert_eq!(
            doc.heading("Class Histogram (by Retained Heap)")
                .map(|h| h.level()),
            Some(3)
        );
        assert_eq!(
            doc.heading("Biggest Packages by Retained Heap")
                .map(|h| h.level()),
            Some(3)
        );
        // Class Histogram lives inside System Overview's body.
        assert!(doc
            .section("System Overview")
            .unwrap()
            .body_contains("### Class Histogram (by Retained Heap)"));
    }

    #[test]
    fn test_render_markdown_oom_triage() {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);
        let doc = Md::parse(&md);

        // (a) new OOM-triage heading + headline retainer line present.
        let triage = doc
            .section("OOM Triage")
            .expect("missing OOM Triage heading");
        assert_eq!(triage.level(), 2, "OOM Triage should be an H2 section");
        // The headline retainer is a bullet, not just loose text.
        assert!(
            triage.has_bullet_starting_with("**Headline retainer:**"),
            "missing headline retainer bullet"
        );
        // Fixture's #1 suspect is com.foo.A (a single object) at 1000/270 -> dominates.
        assert!(
            triage.has_bullet_containing("`com.foo.A`"),
            "headline should name the #1 suspect"
        );
        assert!(
            triage.has_bullet_containing("highly concentrated"),
            "1000/270 is >= 50% so it should read as highly concentrated"
        );

        // The triage block must precede System Overview.
        let tri = doc.heading_offset("OOM Triage").unwrap();
        let sys = doc.heading_offset("System Overview").unwrap();
        assert!(tri < sys, "OOM Triage must come before System Overview");

        // (b) determinism guard: render twice == identical.
        assert_eq!(md, render_markdown(&r));

        // (c) data-preservation: all key section headings still present.
        for needle in [
            "System Overview",
            "Class Histogram",
            "Leak Suspects",
            "Top Consumers",
            "Biggest Objects",
            "Biggest Classes",
            "Biggest Packages",
        ] {
            assert!(doc.heading(needle).is_some(), "missing section: {needle}");
        }
    }

    // ── Phase B: JSON / schema conformance ─────────────────────────────────

    /// Build the fixture Report with the nondeterministic timestamp neutralised.
    fn fixture_report() -> Report {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        r.generated = "FIXED".to_string();
        r
    }

    #[test]
    fn json_round_trip() {
        let mut r = fixture_report();
        let json = serde_json::to_string(&r).expect("serialize");
        let back: Report = serde_json::from_str(&json).expect("deserialize");
        // ObjRow::pct is #[serde(skip)] (f64 kept out of JSON), so it
        // deserializes to its Default (0.0). Zero it on the original before
        // comparing; every OTHER field must survive the round trip.
        for row in &mut r.top.biggest_objects {
            row.pct = 0.0;
        }
        assert_eq!(r, back, "round-tripped Report must equal the original");
    }

    #[test]
    fn render_markdown_round_trips_through_json() {
        // Proves the --render offline path is faithful: serializing a Report to
        // JSON and deserializing it back must produce byte-identical Markdown.
        let r = fixture_report();
        let json = serde_json::to_string(&r).expect("serialize");
        let back: Report = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            render_markdown(&r),
            render_markdown(&back),
            "render_markdown must be stable across a JSON round trip"
        );
    }

    #[test]
    fn json_serialization_is_deterministic() {
        let r = fixture_report();
        let a = serde_json::to_string_pretty(&r).unwrap();
        let b = serde_json::to_string_pretty(&r).unwrap();
        assert_eq!(
            a, b,
            "serializing the same Report twice must be byte-identical"
        );
    }

    #[test]
    fn json_validates_against_schema() {
        let r = fixture_report();
        let instance = serde_json::to_value(&r).expect("Report -> Value");
        let schema = serde_json::to_value(schemars::schema_for!(Report)).expect("schema -> Value");
        let validator = jsonschema::validator_for(&schema).expect("compile schema (draft 2020-12)");
        assert!(
            validator.validate(&instance).is_ok(),
            "serialized fixture Report must validate against schema_for!(Report)"
        );
    }

    #[test]
    fn emit_schema_matches_committed_file() {
        let committed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/schema/report.schema.json"
            ))
            .expect("read committed schema"),
        )
        .expect("parse committed schema");
        let fresh = serde_json::to_value(schemars::schema_for!(Report)).expect("fresh schema");
        // Value-equality: whitespace / key ordering must not cause false diffs.
        assert_eq!(
            committed, fresh,
            "schema/report.schema.json must equal a fresh schema_for!(Report);              regenerate via `dev emit-schema` if the model changed"
        );
    }

    #[test]
    fn schema_version_guard() {
        let r = fixture_report();
        assert_eq!(r.schema_version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 3);
    }

    #[test]
    fn thread_overview_resolves_class_from_object_index() {
        // Two objects: idx 0 is an instance of class row 1 ("java/lang/Thread"),
        // idx 1 is unreachable filler. A thread stack points at obj idx 0.
        let (mut g, _o, _t) = make_graph(
            vec![2, 2],
            vec![1, 0],
            vec![16, 16],
            vec![16, 16],
            vec!["Filler", "java/lang/Thread"],
            &[],
            &[],
            vec![],
            0,
        );
        g.thread_stacks = vec![
            crate::pass2::ThreadStack {
                thread_serial: 7,
                thread_obj_idx: 0,
                frames: vec!["java.lang.Object.wait (Object.java:1)".to_string()],
            },
            crate::pass2::ThreadStack {
                thread_serial: 9,
                thread_obj_idx: u32::MAX,
                frames: vec!["x.y (Unknown Source)".to_string()],
            },
        ];
        let ov = build_thread_overview(&g);
        assert_eq!(ov.threads.len(), 2);
        assert_eq!(ov.threads[0].thread_serial, 7);
        assert_eq!(
            ov.threads[0].class_name.as_deref(),
            Some("java/lang/Thread")
        );
        assert_eq!(ov.threads[0].frames.len(), 1);
        // Unresolved object index yields no class name.
        assert_eq!(ov.threads[1].class_name, None);
    }

    #[test]
    fn render_threads_emits_heading_and_frames() {
        let mut out = String::new();
        render_threads(
            &ThreadOverview {
                threads: vec![ThreadInfo {
                    thread_serial: 3,
                    name: Some("main".to_string()),
                    class_name: Some("java/lang/Thread".to_string()),
                    frames: vec!["java.lang.Object.wait (Object.java:1)".to_string()],
                    local_root_count: 0,
                    local_objects: None,
                    shallow: 104,
                    retained: 200,
                    max_local_retained: 0,
                    context_class_loader: None,
                    is_daemon: false,
                    priority: 5,
                    thread_state: "[alive, runnable]".to_string(),
                    significant_frames: vec![],
                }],
            },
            false,
            &mut out,
        );
        assert!(out.contains("## Threads"));
        assert!(out.contains("### Thread 3 \"main\" (java/lang/Thread)"));
        assert!(out.contains("java.lang.Object.wait (Object.java:1)"));
    }

    #[test]
    fn render_threads_handles_empty() {
        let mut out = String::new();
        render_threads(&ThreadOverview { threads: vec![] }, false, &mut out);
        assert!(out.contains("## Threads"));
        assert!(out.contains("No thread call stacks"));
    }

    #[test]
    fn test_top_size_distribution() {
        // DESC-sorted retained sizes.
        let d = build_size_distribution(&[1000, 500, 500, 8, 3]);
        assert_eq!(d.count, 5);
        assert_eq!(d.max, 1000);
        assert_eq!(d.min, 3);
        assert_eq!(d.total, 2011);
        // median = middle element of a 5-element DESC slice = index 2 = 500.
        assert_eq!(d.median, 500);
        // 3->4, 8->8, 500->512 (x2), 1000->1024, ascending.
        assert_eq!(
            d.buckets,
            vec![
                SizeBucket {
                    upper_bytes: 4,
                    count: 1
                },
                SizeBucket {
                    upper_bytes: 8,
                    count: 1
                },
                SizeBucket {
                    upper_bytes: 512,
                    count: 2
                },
                SizeBucket {
                    upper_bytes: 1024,
                    count: 1
                },
            ]
        );

        // Empty slice -> default (all-zero) distribution.
        let empty = build_size_distribution(&[]);
        assert_eq!(empty, TopSizeDistribution::default());
    }

    /// Container attribution is None on the default path (flag off, raw vec
    /// absent) so the JSON field stays absent — byte-identical to today. When
    /// the raw vec is present (even empty) it becomes Some so the key appears.
    #[test]
    fn test_collection_attribution_none_and_some() {
        // None path: fixture graph has collection_attribution_raw == None.
        let (mut g, dc_off, dc_tgt) = fixture();
        assert!(g.collection_attribution_raw.is_none());
        let r = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        assert!(
            r.collection_attribution.is_none(),
            "flag-off must leave collection_attribution absent"
        );

        // Some(empty) path: build_model must emit Some with empty rankings.
        g.collection_attribution_raw = Some(Vec::new());
        let r2 = build_model_t(&g, &dc_off, &dc_tgt, DOMINATED_CAP);
        let ca = r2
            .collection_attribution
            .expect("Some when raw vec present");
        assert!(ca.most_overall.is_empty());
        assert!(ca.biggest_single.is_empty());
        assert!(!ca.truncated);
    }
}
