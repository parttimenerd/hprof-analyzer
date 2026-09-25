//! Analysis options and detail-level presets, shared between the CLI binary
//! and the WASM library crate.

/// Controls the capture tier for --obj-graph and the collection-detail caps
/// for --collections / --full-analysis. Larger tiers produce richer reports
/// (more element-type samples, more collections tracked, more holder edges)
/// at the cost of more RSS and wall time during the analysis.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum ReportSize {
    Small, // obj-graph: edge_cap=100; collections: minimum-RSS caps
    #[default]
    Default, // obj-graph: edge_cap=150; collections: balanced caps (default)
    Large, // obj-graph: edge_cap=300; collections: 2× balanced caps
    Max,   // obj-graph: edge_cap=500; collections: original caps (most detail)
}

impl ReportSize {
    pub fn edge_cap(self) -> usize {
        match self {
            ReportSize::Small => 100,
            ReportSize::Default => 150,
            ReportSize::Large => 300,
            ReportSize::Max => 500,
        }
    }
    pub fn tier_name(self) -> &'static str {
        match self {
            ReportSize::Small => "small",
            ReportSize::Default => "default",
            ReportSize::Large => "large",
            ReportSize::Max => "max",
        }
    }

    /// Max holder→pointee edges collected under --collections (16 B each).
    pub fn field_ref_cap(self) -> usize {
        match self {
            ReportSize::Small => 1_000_000,   //  16 MB
            ReportSize::Default => 2_500_000, //  40 MB
            ReportSize::Large => 5_000_000,   //  80 MB
            ReportSize::Max => 10_000_000,    // 160 MB (original)
        }
    }

    /// Max container records collected under --collections.
    pub fn container_cap(self) -> usize {
        match self {
            ReportSize::Small => 150_000,
            ReportSize::Default => 375_000,
            ReportSize::Large => 750_000,
            ReportSize::Max => 1_500_000, // original
        }
    }

    /// Max node/entry wrapper objects stored in the node-KV map.
    pub fn node_kv_cap(self) -> usize {
        match self {
            ReportSize::Small => 500_000,
            ReportSize::Default => 1_250_000,
            ReportSize::Large => 2_500_000,
            ReportSize::Max => 5_000_000, // original
        }
    }

    /// Max element slots sampled per collection for the value-type breakdown.
    pub fn coll_values_per_collection(self) -> usize {
        match self {
            ReportSize::Small => 64,
            ReportSize::Default => 256,
            ReportSize::Large => 512,
            ReportSize::Max => 4_096, // original
        }
    }

    /// Max distinct collections whose element types are tallied.
    pub fn coll_values_group_cap(self) -> usize {
        match self {
            ReportSize::Small => 10_000,
            ReportSize::Default => 50_000,
            ReportSize::Large => 100_000,
            ReportSize::Max => 200_000, // original
        }
    }
}

/// Output format for the analysis report.
#[derive(Clone, Copy, PartialEq)]
pub enum OutputFormat {
    /// Human-readable Markdown.
    Md,
    /// Markdown with embedded graph/chart blocks.
    MdGraphs,
    /// Canonical Report JSON (deterministic field order).
    Json,
    /// Standalone HTML.
    Html,
}

/// Default depth cap for bounded `path()` walks in edge-query planning; the
/// `--query-path-depth` flag overrides it. Aliases `query::DEFAULT_PATH_DEPTH_CAP`
/// so there is exactly one numeric source of truth.
pub const DEFAULT_QUERY_PATH_DEPTH: usize = crate::query::DEFAULT_PATH_DEPTH_CAP;

/// Always-applied per-analysis caps, populated from `--detail`. All four heavy
/// analyses (root paths, alloc sites, thread locals, dominator tree) now run
/// unconditionally; these caps bound their output size. `--detail default`
/// reproduces the historical cap values so MAT/golden parity is unchanged.
#[derive(Clone)]
pub struct AnalyzeOptions {
    pub root_path_max_depth: usize,
    pub alloc_sites_top: usize,
    pub thread_locals_per_thread: usize,
    pub dominator_tree_max_nodes: usize,
    pub dominator_tree_max_depth: usize,
    pub leak_children_cap: usize,
    pub top_consumers: usize,
    /// How many histogram rows (sorted by retained desc) get a root-path chain.
    /// Capped to avoid O(k×n) scan regression on dumps with many unique classes.
    pub hist_root_path_top: usize,
    pub find_duplicates: bool,
    pub collections: bool,
    pub collection_config: Option<std::path::PathBuf>,
    pub(crate) coll_descs: Vec<crate::pass2::CollDesc>,
    /// Inline OQL query strings (from `--query`, repeatable).
    pub queries: Vec<String>,
    /// Optional file of OQL queries, one per non-empty/non-comment line.
    pub query_file: Option<String>,
    /// Max hops for OQL `path(a, b)` bounded walks (always > 0).
    pub query_path_depth: usize,
    /// Restrict OQL result rows to GC-reachable objects (Eclipse MAT parity).
    /// The `query` subcommand sets this by default; analyze leaves it off so the
    /// report stays byte-identical unless `--reachable-only` is passed.
    pub reachable_only: bool,
    /// Store field-name labels on forward edges so root-path steps show
    /// `ParentClass.fieldName → ChildClass`. Gated: adds ~2 bytes per edge
    /// (~100–500 MB extra RSS on multi-GB dumps).
    pub ref_paths: bool,
    /// Capture outbound-reference graph + dominator subtree for the top biggest
    /// objects. Enables click-through in the HTML report. Adds ~1-3 MB to the
    /// report JSON; captured in ~30 MB of peak RAM freed after build_model.
    pub obj_graph: bool,
    /// Capture tier for --obj-graph: controls edge_cap per object.
    /// small=100 edges (default), medium=150, large=300.
    pub report_size: ReportSize,
    /// Embed the React bundle as an uncompressed inline <script> in the HTML
    /// report so it is human-readable and editable in DevTools. Output is much
    /// larger but easier to inspect/modify. Implies HTML output format.
    pub dev_report: bool,
    /// Read the React bundle from this path at runtime instead of using the
    /// compile-time embedded bytes. Lets JS/CSS changes take effect without
    /// rebuilding the binary. Only meaningful with dev_report=true.
    pub bundle_path: Option<std::path::PathBuf>,
    /// Skip build_model + render. Used by `mat caches` which discards the report.
    pub skip_report: bool,
    /// Compute per-class reference-field statistics (null/non-null counts, total
    /// retained size of pointees). Opt-in; off by default. Adds O(n) pass.
    pub field_stats: bool,
}

impl Default for AnalyzeOptions {
    /// The `--detail default` preset (historical cap values).
    fn default() -> Self {
        DetailLevel::Default.options()
    }
}

/// Output-size preset. `Default` reproduces the historical cap values so
/// MAT/golden parity is unchanged; `Minimal`/`Max` scale the caps down/up.
#[derive(Clone, Copy, PartialEq)]
pub enum DetailLevel {
    Minimal,
    Default,
    Max,
}

impl DetailLevel {
    pub fn options(self) -> AnalyzeOptions {
        // (root_depth, alloc_top, thread_locals, dom_nodes, dom_depth,
        //  leak_children, top_consumers, hist_root_path_top)
        let (rd, at, tl, dn, dd, lc, tc, hrpt) = match self {
            DetailLevel::Minimal => (10, 15, 5, 500, 10, 15, 10, 10),
            DetailLevel::Default => (30, 50, 20, 5000, 20, 50, 20, 20),
            DetailLevel::Max => (200, 500, 100, 100_000, 50, 500, 100, 100),
        };
        AnalyzeOptions {
            root_path_max_depth: rd,
            alloc_sites_top: at,
            thread_locals_per_thread: tl,
            dominator_tree_max_nodes: dn,
            dominator_tree_max_depth: dd,
            leak_children_cap: lc,
            top_consumers: tc,
            hist_root_path_top: hrpt,
            find_duplicates: false,
            collections: false,
            collection_config: None,
            coll_descs: Vec::new(),
            queries: Vec::new(),
            query_file: None,
            query_path_depth: DEFAULT_QUERY_PATH_DEPTH,
            reachable_only: false,
            ref_paths: false,
            obj_graph: false,
            report_size: ReportSize::Default,
            dev_report: false,
            bundle_path: None,
            skip_report: false,
            field_stats: false,
        }
    }
}
