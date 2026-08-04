//! Analysis options and detail-level presets, shared between the CLI binary
//! and the WASM library crate.

/// Controls the capture tier for --obj-graph: how many edges per object are included.
/// Larger tiers produce bigger HTML reports but cover more of the heap.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum ReportSize {
    #[default]
    Small, // edge_cap=100  (default, current behaviour)
    Medium, // edge_cap=150
    Large,  // edge_cap=300
}

impl ReportSize {
    pub fn edge_cap(self) -> usize {
        match self {
            ReportSize::Small => 100,
            ReportSize::Medium => 150,
            ReportSize::Large => 300,
        }
    }
    pub fn tier_name(self) -> &'static str {
        match self {
            ReportSize::Small => "small",
            ReportSize::Medium => "medium",
            ReportSize::Large => "large",
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
            report_size: ReportSize::Small,
            dev_report: false,
            skip_report: false,
            field_stats: false,
        }
    }
}
