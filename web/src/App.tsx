import React from "react";
import DataTable from "react-data-table-component";
import type { TableColumn } from "react-data-table-component";
import type { AllocSites, ArraysBySize, BiggestCollectionRow, BiggestCollections, ClassRow, CollectionAttribution, CollectionContents, CollectionsAnalysis, Component, DominatorAnalysis, DuplicateClass, FieldsBySize, FillRatioBucket, HeapComposition, HistRow, KindStat, LeakIndicators, LoaderRollup, MergedPathNode, ObjRow, PackageNode, QueryResult, QueryValue, ReferencesAnalysis, ReferenceStats, RefStatClassRow, Report, RootPathStep, SeriesClassRow, SeriesDiffResult, SeriesSuspectRow, Suspect, SystemOverview, ThreadInfo, ThreadLocalObj, TopArrays, TopComponents, UnreachableClassRow } from "./types";
import { fmtCount, fmtExactBytes, fmtPct, formatBytes, formatBytesKB, formatEpochMs, pctOf, shortLoader } from "./format";
import {
  CompositionStackedBar,
  ConcentrationChart,
  ConcentrationStackedBar,
  DepthHistogramChart,
  GcRootsChart,
  HeapCompositionChart,
  LeakShareChart,
  LoaderRollupChart,
  QueryViz,
  TopClassesChart,
  TreemapBar,
  RetainedTreemap,
} from "./charts";
import { UnreachableDomTreeSection, DomSubtreeSvg } from "./domTree";

// ── Theme Toggle ─────────────────────────────────────────────────────────────
// Cycles auto → light → dark → auto. Persists the choice in localStorage so it
// survives page reloads. Uses data-theme on <html> so CSS vars override the OS
// media query only when a manual choice is in effect.
type ThemeMode = "auto" | "light" | "dark";

const CYCLE: Record<ThemeMode, ThemeMode> = { auto: "light", light: "dark", dark: "auto" };
const GLYPHS: Record<ThemeMode, string> = { auto: "◐", light: "☀", dark: "☾" };

function applyMode(m: ThemeMode) {
  if (m === "auto") {
    document.documentElement.removeAttribute("data-theme");
    try { localStorage.removeItem("hprof-theme"); } catch (_) { /* file:// storage may throw */ }
  } else {
    document.documentElement.dataset.theme = m;
    try { localStorage.setItem("hprof-theme", m); } catch (_) { /* file:// storage may throw */ }
  }
}

function ThemeToggle() {
  const [mode, setMode] = React.useState<ThemeMode>("auto");

  React.useEffect(() => {
    try {
      const saved = localStorage.getItem("hprof-theme");
      if (saved === "light" || saved === "dark") {
        setMode(saved);
        applyMode(saved);
      }
    } catch (_) { /* file:// storage may throw */ }
  }, []);

  const next = CYCLE[mode];
  return (
    <button
      className="theme-toggle"
      aria-label={"Theme: " + mode}
      onClick={() => { applyMode(next); setMode(next); }}
    >
      {GLYPHS[mode]} Theme: {mode.charAt(0).toUpperCase() + mode.slice(1)}
    </button>
  );
}

// ── Table expansion context ───────────────────────────────────────────────────
// A global toggle that makes every capped table expand all its rows at once.
const TableExpansionCtx = React.createContext(false);

// ── Per-table KB toggle ───────────────────────────────────────────────────────
// Returns [fmtB, toggleBtn, useKB] — the byte formatter, a button that switches
// between auto-scaled (1.2 MB) and always-KB display, and the current mode flag.
function useFmtBytes(): [(n: number) => string, React.ReactNode, boolean] {
  const [useKB, setUseKB] = React.useState(false);
  const fmtB = useKB ? formatBytesKB : formatBytes;
  const btn = (
    <button className="show-more-btn" onClick={() => setUseKB(v => !v)}
      title="Toggle byte display: auto-scaled vs always-KB">
      {useKB ? "Show as B, KB, …" : "Show as KB"}
    </button>
  );
  return [fmtB, btn, useKB];
}

// Returns a DataTable `cell` renderer for a byte value column.
// In KB mode: shows plain number (no suffix) with exact bytes as title tooltip.
// In normal mode: shows auto-scaled value (e.g. "1.2 MB").
function byteCell<T>(selector: (row: T) => number, fmtB: (n: number) => string, useKB: boolean): (row: T) => React.ReactNode {
  return (row: T) => {
    const raw = selector(row);
    if (useKB) {
      return <span title={fmtExactBytes(raw)}>{fmtB(raw)}</span>;
    }
    return fmtB(raw);
  };
}

const TABLE_CAP = 20;

function useCapped<T>(items: T[], cap = TABLE_CAP): {
  visible: T[];
  hasMore: boolean;
  extra: number;
  showAll: boolean;
  setShowAll: (v: boolean) => void;
} {
  const expandAll = React.useContext(TableExpansionCtx);
  const [showAll, setShowAll] = React.useState(false);
  const open = expandAll || showAll;
  return {
    visible: open ? items : items.slice(0, cap),
    hasMore: items.length > cap,
    extra: items.length - cap,
    showAll: open,
    setShowAll,
  };
}

function ShowMoreRow({ extra, cols, showAll, setShowAll }: { extra: number; cols: number; showAll: boolean; setShowAll: (v: boolean) => void }) {
  if (extra <= 0) return null;
  return (
    <tr>
      <td colSpan={cols} style={{ textAlign: "center", padding: "0.4rem 0" }}>
        {showAll ? (
          <button className="show-more-btn" onClick={() => setShowAll(false)}>Collapse</button>
        ) : (
          <button className="show-more-btn" onClick={() => setShowAll(true)}>Show {fmtCount(extra)} more</button>
        )}
      </td>
    </tr>
  );
}

// A capped <tbody> for tables whose <tfoot> totals must reflect the FULL row
// set. Renders only the first `cap` rows (unless expanded, per-table or via the
// global expand-all toggle) plus a "Show N more" row, while the caller keeps
// computing totals over the complete array. `cols` is the column count for the
// ShowMoreRow's colSpan.
function CappedTbody<T>({ rows, cols, renderRow, cap = TABLE_CAP }: {
  rows: T[];
  cols: number;
  renderRow: (row: T, i: number) => React.ReactNode;
  cap?: number;
}) {
  const { visible, extra, showAll, setShowAll } = useCapped(rows, cap);
  return (
    <tbody>
      {visible.map(renderRow)}
      <ShowMoreRow extra={extra} cols={cols} showAll={showAll} setShowAll={setShowAll} />
    </tbody>
  );
}

// StdTable — standard table with filter toolbar + DataTable + show-more.
// searchKeys: row field names to match filter text against. Pass [] to hide search.
function StdTable<T extends object>({
  columns, data, searchKeys = [], keyField,
  defaultSortFieldId, defaultSortAsc = false,
  fmtBtn, extraBtns, cap = TABLE_CAP,
}: {
  columns: TableColumn<T>[];
  data: T[];
  searchKeys?: (keyof T & string)[];
  keyField?: string;
  defaultSortFieldId?: string;
  defaultSortAsc?: boolean;
  fmtBtn?: React.ReactNode;
  extraBtns?: React.ReactNode;
  cap?: number;
}) {
  const [filter, setFilter] = React.useState("");
  const filtered = React.useMemo(() => {
    if (!searchKeys.length || !filter) return data;
    const lc = filter.toLowerCase();
    return data.filter(row => searchKeys.some(k => String((row as any)[k] ?? "").toLowerCase().includes(lc)));
  }, [data, filter, searchKeys]);
  const { visible, extra, showAll, setShowAll } = useCapped(filtered, cap);
  const hasToolbar = searchKeys.length > 0 || fmtBtn || extraBtns;
  return (
    <>
      {hasToolbar && (
        <div className="tools">
          {searchKeys.length > 0 && (
            <>
              <input type="text" className="filter" placeholder="Filter…" value={filter}
                onChange={e => setFilter(e.target.value)} />
              {filter && <span className="hint">{fmtCount(filtered.length)} shown</span>}
            </>
          )}
          {extraBtns}
          {fmtBtn}
        </div>
      )}
      <DataTable columns={columns} data={visible} keyField={keyField}
        defaultSortFieldId={defaultSortFieldId} defaultSortAsc={defaultSortAsc}
        customStyles={histogramTableStyles} dense highlightOnHover />
      {extra > 0 && (
        <button className="show-more-btn" onClick={() => setShowAll(!showAll)}>
          {showAll ? "Collapse" : `Show ${fmtCount(extra)} more`}
        </button>
      )}
    </>
  );
}

// ── Navigation ───────────────────────────────────────────────────────────────
// A sticky in-page table of contents so long reports (hundreds of threads,
// thousands of histogram rows) stay navigable — MAT's report has an equivalent
// left-hand section index.
function Nav({ report }: { report: Report }) {
  // [id, label, group?, badge?] — group is set only on the first link of each group.
  const items: [string, string, (string | undefined)?, (string | undefined)?][] = [];

  // ── Overview group ──
  items.push(
    ["memory-triage",  "Memory Triage",      "Overview"],
  );
  if (report.waste_summary && report.waste_summary.total_bytes > 0) {
    items.push(["waste-summary", "Waste Summary"]);
  }
  items.push(
    ["system-overview", "System Overview"],
    ["hprof-record-census", "HPROF Record Census"],
  );

  // ── Analysis group ──
  const suspectCount = report.leaks.suspects.length;
  const threadCount = report.threads?.threads?.length ?? 0;
  items.push(["leak-suspects",       "Leak Suspects",    "Analysis", suspectCount > 0 ? String(suspectCount) : undefined]);
  items.push(["top-consumers",       "Top Consumers"]);
  items.push(["dominator-analysis", "Dominator Analysis"]);
  items.push(["threads",            "Threads", undefined, threadCount > 0 ? String(threadCount) : undefined]);
  if (report.top.size_distribution.count > 0) items.push(["size-distribution", "Size Distribution"]);

  // ── Data group ──
  let dataGroupSet = false;
  const addData = (id: string, label: string, badge?: string) => {
    if (!dataGroupSet) { items.push([id, label, "Data", badge]); dataGroupSet = true; }
    else items.push([id, label, undefined, badge]);
  };
  if (report.overview.duplicate_strings) addData("duplicate-strings-approximate", "Duplicate Strings");
  if (report.overview.duplicate_prim_arrays) addData("duplicate-prim-arrays", "Duplicate Prim Arrays");
  if (report.overview.boxed_numbers?.length) addData("boxed-numbers", "Boxed Numbers");
  if (report.overview.header_overhead?.length) addData("object-header-overhead", "Header Overhead");
  if (report.top_components?.components?.length) addData("top-components", "Top Components");
  addData("arrays-by-size", "Arrays by Size");
  addData("collections", "Collections");
  if (report.collection_attribution) addData("container-attribution-classfield", "Container Attribution");
  if (report.fields_by_size) addData("fields-by-retained-size-classfield", "Fields by Size");
  if (report.top_retainers?.length) addData("top-retainers", "Top Retainers");
  if (report.biggest_collections) addData("biggest-collections", "Biggest Collections");
  if (report.collection_contents) addData("collection-contents-by-type", "Collection Contents");
  addData("references", "References");
  addData("unreachable-objects", "Unreachable Objects");
  if (report.alloc_sites) addData("alloc-sites", "Allocation Sites");
  if (report.queries?.length) addData("custom-queries", "Custom Queries", String(report.queries.length));

  // ── Distribution group ──
  let distGroupSet = false;
  const addDist = (id: string, label: string) => {
    if (!distGroupSet) { items.push([id, label, "Distribution"]); distGroupSet = true; }
    else items.push([id, label]);
  };
  const rc = report.overview.retention_concentration;
  if (rc.top1_bp > 0 || rc.num_objects_ge_1pct > 0) addDist("retention-concentration", "Retention Concentration");
  if (report.overview.dominator_depth_histogram.length > 0) addDist("dominator-depth-distribution", "Dominator-Depth Distribution");
  const li = report.leak_indicators;
  if (li && (li.anonymous_class_count > 0 || li.thread_local_null_key_count > 0 || li.direct_byte_buffer_capacity_sum > 0)) {
    addDist("leak-indicators", "Leak Indicators");
  }
  addDist("glossary", "Glossary");

  const [active, setActive] = React.useState<string>("");

  React.useEffect(() => {
    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((e) => {
          intersecting.set(e.target.id, e.isIntersecting);
        });
        const ids = items.map(([id]) => id);
        let chosen = "";
        let lowestAbove = -Infinity;
        for (const id of ids) {
          const el = document.getElementById(id);
          if (!el) continue;
          const top = el.getBoundingClientRect().top;
          if (intersecting.get(id)) { chosen = id; break; }
          if (top < 0 && top > lowestAbove) { lowestAbove = top; chosen = id; }
        }
        setActive(chosen);
      },
      { rootMargin: "-40% 0px -55% 0px" },
    );
    const intersecting = new Map<string, boolean>();
    items.forEach(([id]) => { const el = document.getElementById(id); if (el) observer.observe(el); });
    return () => observer.disconnect();
  }, []);

  return (
    <nav className="toc">
      {items.map(([id, label, group, badge]) => (
        <React.Fragment key={id}>
          {group && <span className="toc-group">{group}</span>}
          <a href={`#${id}`} className={id === active ? "active" : ""}>
            {label}
            {badge && <span className="toc-badge">{badge}</span>}
          </a>
        </React.Fragment>
      ))}
    </nav>
  );
}

// ── Back-to-top button ───────────────────────────────────────────────────────
function BackToTop() {
  const [visible, setVisible] = React.useState(false);

  React.useEffect(() => {
    const onScroll = () => setVisible(window.scrollY > 600);
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

  if (!visible) return null;
  return (
    <button
      className="back-to-top"
      aria-label="Back to top"
      onClick={() => window.scrollTo({ top: 0, behavior: "smooth" })}
    >
      ↑
    </button>
  );
}

// ── OOM Triage lead-in ──────────────────────────────────────────────────────
// Dumb formatter over report.triage (rules are evaluated once in Rust; see
// src/report/triage.rs). Mirrors render_markdown's render_oom_triage.

// Split a detail string on backtick code spans, rendering `x` as <code>x</code>.
function InlineCode({ text }: { text: string }) {
  const parts = text.split("`");
  return (
    <>
      {parts.map((p, i) =>
        i % 2 === 1 ? <code key={i}>{p}</code> : <React.Fragment key={i}>{p}</React.Fragment>,
      )}
    </>
  );
}

function OomTriage({ report }: { report: Report }) {
  const signals = report.triage ?? [];
  return (
    <div className="oom" id="memory-triage" tabIndex={-1}>
      <h2>Memory Triage</h2>
      <p className="subtitle">Where the reachable heap is concentrated, at a glance.</p>
      <ul>
        {signals.map((s, i) => (
          <li key={i}>
            <strong>{s.title}:</strong> <InlineCode text={s.detail} />
            {s.anchor && s.anchor_label ? (
              <>
                {" "}
                See <a href={`#${s.anchor}`}>{s.anchor_label}</a>.
              </>
            ) : null}
          </li>
        ))}
      </ul>
    </div>
  );
}

// ── Waste Summary ─────────────────────────────────────────────────────────
// One headline "reclaimable N" figure folding every quantifiable waste source,
// with a per-source breakdown that links into the section detailing each.
// Sources are approximate and may overlap slightly. Mirrors the Rust md/graphs
// "Waste Summary" section (same order, same values).
function WasteSummarySection({ report }: { report: Report }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const w = report.waste_summary;
  if (!w || w.total_bytes <= 0) return null;
  const max = w.sources.reduce((m, s) => Math.max(m, s.bytes), 0);
  // Anchors the Rust side omits (subsections without a dedicated section id).
  const wasteAnchorFallback: Record<string, string> = {
    "Duplicate primitive arrays": "duplicate-prim-arrays",
  };
  type WasteSource = (typeof w.sources)[0];
  const wasteCols: TableColumn<WasteSource>[] = [
    {
      id: "source", name: "Source", grow: 1,
      cell: (s) => {
        const anchor = s.anchor ?? wasteAnchorFallback[s.label];
        return anchor ? <a href={`#${anchor}`}>{s.label}</a> : s.label;
      },
    },
    { id: "reclaimable", name: useKB ? "Reclaimable (KB)" : "Reclaimable", right: true, width: useKB ? "150px" : "120px", cell: byteCell(s => s.bytes, fmtB, useKB), selector: (s) => s.bytes },
    {
      id: "bar", name: "", width: "100px",
      cell: (s) => (
        <span className="bar-bg">
          <span className="bar-fill" style={{ width: `${max > 0 ? (s.bytes / max) * 100 : 0}%` }} />
        </span>
      ),
    },
  ];
  return (
    <section className="section" id="waste-summary" tabIndex={-1}>
      <h2>Waste Summary</h2>
      <p className="subtitle">
        Approximately <strong>{fmtB(w.total_bytes)}</strong> looks reclaimable across the
        sources below. Figures are approximate and may overlap slightly.
      </p>
      <StdTable columns={wasteCols} data={w.sources} searchKeys={["label"]} fmtBtn={kbBtn} />
    </section>
  );
}

// ── KPI card strip ──────────────────────────────────────────────────────────
function KpiStrip({ report }: { report: Report }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const suspects = report.leaks.suspects;
  const top = suspects[0];
  const topShare = top
    ? fmtPct(pctOf(top.retained, report.leaks.total_shallow))
    : "—";
  const dominantClass = top?.pretty_class ?? "—";

  // Plain-language verdict mirroring the Markdown executive summary
  // ("Likely problem:" line). CONCENTRATION_PCT = 50.
  const pct = top ? pctOf(top.retained, report.leaks.total_shallow) : 0;
  let verdict: React.ReactNode;
  if (top && pct >= 50) {
    verdict = (
      <>
        <strong>Likely problem:</strong> <code>{top.pretty_class}</code> retains {fmtPct(pct)} of the reachable heap
        — investigate this first.
      </>
    );
  } else if (top) {
    verdict = (
      <>
        <strong>Likely problem:</strong> retention is spread across several roots; no single object dominates.
      </>
    );
  } else {
    verdict = (
      <>
        <strong>Likely problem:</strong> no dominant retainer; the heap looks evenly distributed.
      </>
    );
  }

  return (
    <>
      <div className="kpi-grid">
      <a className="kpi kpi-link" href="#system-overview" title="Jump to System Overview">
        <div className="kpi-value">{fmtB(report.overview.total_shallow)}</div>
        <div className="kpi-label">Total reachable heap</div>
      </a>
      <a className="kpi kpi-link" href="#system-overview" title="Jump to System Overview">
        <div className="kpi-value">{fmtCount(report.overview.total_objects)}</div>
        <div className="kpi-label">Objects</div>
      </a>
      <a className="kpi kpi-link" href="#leak-suspects" title="Jump to Leak Suspects">
        <div className="kpi-value">{fmtCount(suspects.length)}</div>
        <div className="kpi-label">Leak suspects</div>
      </a>
      <a className="kpi kpi-link" href="#leak-suspects" title="Jump to Leak Suspects">
        <div className="kpi-value">{topShare}</div>
        <div className="kpi-label">Top suspect share</div>
      </a>
      <a className="kpi kpi-link" href="#leak-suspects" title="Jump to Leak Suspects">
        <div className="kpi-value">
          <code title={dominantClass}>{dominantClass}</code>
        </div>
        <div className="kpi-label">Dominant retainer</div>
      </a>
      <a className="kpi kpi-link" href="#system-overview" title="Jump to System Overview">
        <div className="kpi-value">{fmtCount(report.overview.gc_roots)}</div>
        <div className="kpi-label">GC roots</div>
      </a>
      </div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "baseline", flexWrap: "wrap", gap: "0.5rem", marginBottom: "0.5rem" }}>
        <p className="subtitle" style={{ fontSize: "1rem", margin: 0 }}>{verdict}</p>
        {kbBtn}
      </div>
    </>
  );
}

// ── Column-resize hook ───────────────────────────────────────────────────────
// ── Reusable sort primitives ─────────────────────────────────────────────────
function useSortedRows<T>(rows: T[], initialKey: keyof T) {
  const [sortKey, setSortKey] = React.useState<keyof T>(initialKey);
  const sorted = React.useMemo(
    () => [...rows].sort((a, b) => (b[sortKey] as number) - (a[sortKey] as number)),
    [rows, sortKey],
  );
  return { sorted, sortKey, setSortKey };
}

function SortableTh<T>({ label, colKey, sortKey, setSortKey }: {
  label: string; colKey: keyof T; sortKey: keyof T; setSortKey: (k: keyof T) => void;
}) {
  const active = sortKey === colKey;
  const handleKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setSortKey(colKey); }
  };
  return (
    <th
      className={"num sortable" + (active ? " active" : "")}
      onClick={() => setSortKey(colKey)}
      onKeyDown={handleKey}
      tabIndex={0}
      role="button"
      aria-sort={active ? "descending" : "none"}
      title={`Sort by ${label} (descending)`}
    >
      {label} {active ? "▾" : ""}
    </th>
  );
}

// ── Sortable / filterable class histogram ────────────────────────────────────
const HIST_MIN_PCT = 0.1; // skip rows < 0.1% of heap

function ClassHistogramTable({ rows, totalShallow }: { rows: HistRow[]; totalShallow: number }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const [filter, setFilter] = React.useState("");
  const [showAll, setShowAll] = React.useState(false);
  const [showLoader, setShowLoader] = React.useState(false);

  // Only offer the loader toggle if at least one non-boot loader exists
  const hasNonBootLoader = React.useMemo(
    () => rows.some((r) => r.loader_label != null && r.loader_label !== "<boot>"),
    [rows],
  );

  // Filter by class name text + optionally skip tiny rows
  const filtered = React.useMemo(() => {
    const lc = filter.toLowerCase();
    return rows.filter((r) => {
      if (lc && !r.pretty_class.toLowerCase().includes(lc)) return false;
      if (!showAll && pctOf(r.retained, totalShallow) < HIST_MIN_PCT) return false;
      return true;
    });
  }, [rows, filter, showAll, totalShallow]);

  const columns: TableColumn<HistRow>[] = React.useMemo(() => {
    const cols: TableColumn<HistRow>[] = [
      {
        id: "rank",
        name: "#",
        width: "52px",
        grow: 0,
        style: { color: "var(--muted)", fontSize: "0.8rem", justifyContent: "flex-end" },
        cell: (_row, idx) => idx + 1,
        sortable: false,
      },
      {
        id: "pretty_class",
        name: "Class",
        minWidth: "100px",
        grow: 1,
        selector: (r) => r.pretty_class,
        cell: (r) => (
          <span title={r.pretty_class} style={{ display: "flex", alignItems: "center", gap: 4, overflow: "hidden", width: "100%" }}>
            <code style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", flex: 1, minWidth: 0, background: "none", padding: 0 }}>{r.pretty_class}</code>
            <CopyBtn text={r.pretty_class} />
          </span>
        ),
        sortable: false,
      },
      ...(showLoader ? [{
        id: "loader_label",
        name: "Loader",
        width: "130px",
        grow: 0,
        selector: (r: HistRow) => r.loader_label ?? "",
        cell: (r: HistRow) => (
          <span title={r.loader_label ?? undefined} style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", display: "block", width: "100%" }}>
            <LoaderCell label={r.loader_label} />
          </span>
        ),
        sortable: false,
      }] : []),
      {
        id: "instances",
        name: "Instances",
        width: "116px",
        grow: 0,
        right: true,
        selector: (r) => r.instances,
        format: (r) => fmtCount(r.instances),
        sortable: true,
      },
      {
        id: "shallow",
        name: useKB ? "Shallow (KB)" : "Shallow",
        width: useKB ? "122px" : "104px",
        grow: 0,
        right: true,
        selector: (r) => r.shallow,
        cell: byteCell(r => r.shallow, fmtB, useKB),
        sortable: true,
      },
      {
        id: "max_instance_shallow",
        name: useKB ? "Largest (KB)" : "Largest",
        width: useKB ? "122px" : "104px",
        grow: 0,
        right: true,
        selector: (r) => r.max_instance_shallow,
        cell: byteCell(r => r.max_instance_shallow, fmtB, useKB),
        sortable: true,
      },
      {
        id: "retained",
        name: useKB ? "Retained (KB)" : "Retained",
        width: useKB ? "130px" : "112px",
        grow: 0,
        right: true,
        selector: (r) => r.retained,
        cell: byteCell(r => r.retained, fmtB, useKB),
        sortable: true,
      },
      {
        id: "pct",
        name: "% Heap",
        width: "104px",
        grow: 0,
        right: true,
        selector: (r) => r.retained,
        format: (r) => fmtPct(pctOf(r.retained, totalShallow)),
        sortable: true,
        sortFunction: (a, b) => a.retained - b.retained,
      },
    ];
    return cols;
  }, [showLoader, totalShallow, fmtB, useKB]);

  const hiddenSmall = !showAll && !filter ? rows.filter(r => pctOf(r.retained, totalShallow) < HIST_MIN_PCT).length : 0;

  return (
    <div>
      <div className="tools">
        <input
          type="text"
          className="filter"
          placeholder="Filter by class name…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          aria-label="Filter histogram by class name"
        />
        <span className="hint">{fmtCount(filtered.length)} shown</span>
        {hasNonBootLoader && (
          <button className="show-more-btn" onClick={() => setShowLoader(v => !v)}>
            {showLoader ? "Hide Loader" : "Show Loader"}
          </button>
        )}
        {hiddenSmall > 0 && (
          <button className="show-more-btn" onClick={() => setShowAll(true)}>
            + {fmtCount(hiddenSmall)} rows &lt;0.1%
          </button>
        )}
        {showAll && !filter && (
          <button className="show-more-btn" onClick={() => setShowAll(false)}>
            Hide &lt;0.1% rows
          </button>
        )}
        {kbBtn}
      </div>
      <DataTable
        columns={columns}
        data={filtered}
        keyField="pretty_class"
        defaultSortFieldId="retained"
        defaultSortAsc={false}
        dense
        highlightOnHover
        customStyles={histogramTableStyles}
      />
    </div>
  );
}

const histogramTableStyles = {
  headRow: { style: { borderBottomWidth: "1px", borderBottomColor: "var(--border)", fontWeight: 600, fontSize: "0.82rem", color: "var(--muted)", background: "var(--card)" } },
  headCells: { style: { paddingLeft: "5px", paddingRight: "5px", whiteSpace: "nowrap" as const } },
  rows: { style: { fontSize: "0.86rem", borderBottomColor: "var(--border)", background: "transparent", minHeight: "unset" }, highlightOnHoverStyle: { background: "var(--card)" } },
  cells: { style: { paddingTop: "3px", paddingBottom: "3px", paddingLeft: "5px", paddingRight: "5px", whiteSpace: "nowrap" as const, overflow: "hidden", fontVariantNumeric: "tabular-nums" } },
  table: { style: { background: "transparent" } },
  tableWrapper: { style: { overflow: "auto" } },
};

// Renders a class-loader label compactly: the loader's simple class name, with
// the full JVM-internal name as a tooltip. The boot loader is shown muted.
function LoaderCell({ label }: { label?: string | null }) {
  const short = shortLoader(label);
  if (short == null) return <span className="hint">—</span>;
  if (short === "<boot>") return <span className="hint">&lt;boot&gt;</span>;
  return (
    <code className="loader" title={label ?? undefined}>
      {short}
    </code>
  );
}

function CopyBtn({ text }: { text: string }) {
  const [copied, setCopied] = React.useState(false);
  const copy = (e: React.MouseEvent) => {
    e.stopPropagation();
    navigator.clipboard?.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    });
  };
  return (
    <button className="copy-btn" onClick={copy} title="Copy class name" aria-label="Copy class name">
      {copied ? "✓" : "⎘"}
    </button>
  );
}

// ── TextModal ─────────────────────────────────────────────────────────────────
// Full-screen overlay showing long text with a search/highlight bar.
// Uses the native <dialog> element for focus-trap and backdrop.
function TextModal({ title, text, onClose }: { title: string; text: string; onClose: () => void }) {
  const dialogRef = React.useRef<HTMLDialogElement>(null);
  const [query, setQuery] = React.useState("");

  React.useEffect(() => {
    const d = dialogRef.current;
    if (!d) return;
    d.showModal();
    const close = () => onClose();
    d.addEventListener("close", close);
    return () => d.removeEventListener("close", close);
  }, [onClose]);

  const highlighted = React.useMemo(() => {
    if (!query) return <code className="text-modal-body">{text}</code>;
    const lc = query.toLowerCase();
    const parts: React.ReactNode[] = [];
    let i = 0;
    let lcText = text.toLowerCase();
    while (i < text.length) {
      const idx = lcText.indexOf(lc, i);
      if (idx === -1) { parts.push(text.slice(i)); break; }
      if (idx > i) parts.push(text.slice(i, idx));
      parts.push(<mark key={idx}>{text.slice(idx, idx + query.length)}</mark>);
      i = idx + query.length;
    }
    return <code className="text-modal-body">{parts}</code>;
  }, [text, query]);

  return (
    <dialog ref={dialogRef} className="text-modal" onClick={e => { if (e.target === dialogRef.current) dialogRef.current?.close(); }}>
      <div className="text-modal-inner">
        <div className="text-modal-header">
          <span className="text-modal-title">{title}</span>
          <input
            type="text"
            className="filter"
            placeholder="Search…"
            value={query}
            onChange={e => setQuery(e.target.value)}
            autoFocus
          />
          <button className="copy-btn" style={{ fontSize: "1rem" }} onClick={() => navigator.clipboard?.writeText(text)} title="Copy">⎘</button>
          <button className="copy-btn" style={{ fontSize: "1.1rem" }} onClick={() => dialogRef.current?.close()} title="Close">✕</button>
        </div>
        <div className="text-modal-content">
          {highlighted}
        </div>
        {query && (
          <div className="text-modal-footer">
            {(() => {
              const count = (text.toLowerCase().match(new RegExp(query.toLowerCase().replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "g")) || []).length;
              return <span className="hint">{count} match{count !== 1 ? "es" : ""}</span>;
            })()}
          </div>
        )}
      </div>
    </dialog>
  );
}

// ExpandableText: shows text truncated in cell; double-click opens TextModal.
// Any cell text is expandable; long text also shows a small "⤢" hint button.
const EXPAND_THRESHOLD = 60;
function ExpandableText({ text, label }: { text: string; label?: string }) {
  const [open, setOpen] = React.useState(false);
  const isLong = text.length > EXPAND_THRESHOLD;
  return (
    <span
      className="expandable-text"
      onDoubleClick={e => { e.stopPropagation(); setOpen(true); }}
      title={isLong ? "Double-click to expand" : undefined}
    >
      <code className={isLong ? "expandable-truncated" : ""}>{text}</code>
      {isLong && (
        <button
          className="expand-btn"
          onClick={e => { e.stopPropagation(); setOpen(true); }}
          title="Show full value"
        >⤢</button>
      )}
      {open && <TextModal title={label ?? "Full value"} text={text} onClose={() => setOpen(false)} />}
    </span>
  );
}

// ── ChartOrNote ──────────────────────────────────────────────────────────────
// Renders children when hasData is true; otherwise shows a muted note matching
// the "System properties not captured in this dump." pattern.
function ChartOrNote({ hasData, note, children }: { hasData: boolean; note: string; children: React.ReactNode }) {
  if (!hasData) return <p className="subtitle" style={{ color: "var(--muted)" }}>{note}</p>;
  return <>{children}</>;
}

// ── HPROF Record Census ───────────────────────────────────────────────────────
function gcRootTagLabel(tag: number): string {
  switch (tag) {
    case 0x00: return "System Class";
    case 0x01: return "JNI Global";
    case 0x02: return "JNI Local";
    case 0x03: return "Java Frame";
    case 0x04: return "Native Stack";
    case 0x05: return "Sticky Class";
    case 0x06: return "Thread Block";
    case 0x07: return "Busy Monitor";
    case 0x08: return "Thread";
    default: return "Unknown";
  }
}

function RecordCensusSection({ report }: { report: Report }) {
  const c = report.overview.record_census;
  const rows: { label: string; count: number }[] = [
    { label: "UTF8 strings", count: c.utf8_records },
    { label: "Load class", count: c.load_class_records },
    { label: "Unload class", count: c.unload_class_records },
    { label: "Stack frames", count: c.stack_frame_records },
    { label: "Stack traces", count: c.stack_trace_records },
    { label: "Heap dump segments", count: c.heap_dump_segments },
    { label: "Instance dumps", count: c.instance_dumps },
    { label: "Object-array dumps", count: c.obj_array_dumps },
    { label: "Primitive-array dumps", count: c.prim_array_dumps },
    { label: "Class dumps", count: c.class_dumps },
  ];
  const censusCols: TableColumn<{ label: string; count: number }>[] = [
    { id: "label", name: "Record Type", grow: 1, selector: (r) => r.label },
    { id: "count", name: "Count", right: true, width: "120px", format: (r) => fmtCount(r.count), selector: (r) => r.count },
  ];
  return (
    <section id="hprof-record-census">
      <h2>HPROF Record Census</h2>
      <p className="subtitle">
        Raw HPROF record-type composition of the dump (pass-1 counts); additive, not parity-compared.
      </p>
      <StdTable columns={censusCols} data={rows} searchKeys={["label"]} />
    </section>
  );
}

// ── Top-Dominator Size Distribution ───────────────────────────────────────────
function SizeDistributionSection({ report }: { report: Report }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const d = report.top.size_distribution;
  if (d.count <= 0) return null;
  type SizeBucket = (typeof d.buckets)[0];
  const sizeCols: TableColumn<SizeBucket>[] = [
    { id: "upper", name: useKB ? "Size ≤ (KB)" : "Size ≤", right: true, width: useKB ? "140px" : "120px", cell: byteCell(b => b.upper_bytes, fmtB, useKB), selector: (b) => b.upper_bytes },
    { id: "count", name: "Count", right: true, width: "100px", format: (b) => fmtCount(b.count), selector: (b) => b.count },
    { id: "pct", name: "% of Dom.", right: true, width: "100px", format: (b) => d.count > 0 ? fmtPct(b.count / d.count * 100) : "—", selector: (b) => b.count },
  ];
  return (
    <section id="size-distribution">
      <h2>Top-Dominator Size Distribution</h2>
      <p className="subtitle">
        Retained-size spread across all {fmtCount(d.count)} top-level dominators (the biggest memory contributors).
      </p>
      <ul>
        <li>Dominators: {fmtCount(d.count)}</li>
        <li>Smallest / largest retained: {fmtB(d.min)} / {fmtB(d.max)}</li>
        <li>Median retained: {fmtB(d.median)}</li>
        <li>Total retained (top-level): {fmtB(d.total)}</li>
      </ul>
      <StdTable columns={sizeCols} data={d.buckets} searchKeys={[]} fmtBtn={kbBtn} />
      <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
        <span style={{ width: useKB ? "140px" : "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }}>Total</span>
        <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(d.count)}</span>
        <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>100.0%</span>
        <span style={{ flex: 1 }} />
      </div>
    </section>
  );
}

// ── Small capped sub-tables for Duplicate Strings section ────────────────────
function TopDuplicatedTable({ rows }: { rows: DupStringSample[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const cols: TableColumn<DupStringSample>[] = [
    { id: "rank", name: "#", right: true, width: "52px", cell: (_r, i) => (i ?? 0) + 1 },
    { id: "count", name: "Count", right: true, width: "100px", format: (s) => fmtCount(s.count), selector: (s) => s.count },
    { id: "wasted", name: useKB ? "Wasted (KB)" : "Wasted", right: true, width: useKB ? "120px" : "100px", cell: byteCell(s => s.wasted_bytes, fmtB, useKB), selector: (s) => s.wasted_bytes },
    { id: "value", name: "Value", grow: 1, cell: (s) => <ExpandableText text={s.text} label="Duplicated string value" /> },
  ];
  return (
    <>
      <h3>Most-Duplicated Values</h3>
      <StdTable columns={cols} data={rows} searchKeys={["text"]} fmtBtn={kbBtn} />
    </>
  );
}

function TopByLengthTable({ rows }: { rows: DupStringSample[] }) {
  const cols: TableColumn<DupStringSample>[] = [
    { id: "rank", name: "#", right: true, width: "52px", cell: (_r, i) => (i ?? 0) + 1 },
    { id: "len", name: "Length", right: true, width: "100px", format: (s) => fmtCount(s.len), selector: (s) => s.len },
    { id: "count", name: "Count", right: true, width: "100px", format: (s) => fmtCount(s.count), selector: (s) => s.count },
    { id: "value", name: "Value", grow: 1, cell: (s) => <ExpandableText text={s.text} label="Longest string value" /> },
  ];
  return (
    <>
      <h3>Longest Values</h3>
      <StdTable columns={cols} data={rows} searchKeys={["text"]} />
    </>
  );
}

function StringHoldersTable({ rows }: { rows: StringHolder[] }) {
  const cols: TableColumn<StringHolder>[] = [
    { id: "class", name: "Class", grow: 1, cell: (h) => <code>{h.class_name}</code> },
    { id: "refs", name: "String refs", right: true, width: "120px", format: (h) => fmtCount(h.string_refs), selector: (h) => h.string_refs },
  ];
  return (
    <>
      <h3>Classes Holding the Most Strings</h3>
      <p className="subtitle">
        Number of <code>java.lang.String</code> instances referenced by each class's instances.
      </p>
      <StdTable columns={cols} data={rows} searchKeys={["class_name"]} />
    </>
  );
}

function CharArrayWasteTopTable({ rows }: { rows: CharArrayWasteRow[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const cols: TableColumn<CharArrayWasteRow>[] = [
    { id: "array", name: "Array #", right: true, width: "100px", format: (r) => fmtCount(r.array_obj_1based), selector: (r) => r.array_obj_1based },
    { id: "length", name: "Length", right: true, width: "100px", format: (r) => fmtCount(r.length), selector: (r) => r.length },
    { id: "used", name: useKB ? "Used (KB)" : "Used", right: true, width: useKB ? "120px" : "100px", cell: byteCell(r => r.used, fmtB, useKB), selector: (r) => r.used },
    { id: "wasted", name: useKB ? "Wasted (KB)" : "Wasted", right: true, width: useKB ? "120px" : "100px", cell: byteCell(r => r.wasted_bytes, fmtB, useKB), selector: (r) => r.wasted_bytes },
  ];
  return (
    <StdTable columns={cols} data={rows} searchKeys={[]} fmtBtn={kbBtn} />
  );
}

// ── Small capped sub-tables for Duplicate Prim Arrays section ─────────────────
function DupPrimArrayRowsTable({ rows }: { rows: DupPrimArrayRow[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const cols: TableColumn<DupPrimArrayRow>[] = [
    { id: "rank", name: "#", right: true, width: "52px", cell: (_r, i) => (i ?? 0) + 1 },
    { id: "type", name: "Array type", grow: 1, cell: (r) => <code>{r.array_class}</code> },
    { id: "groups", name: "Dup groups", right: true, width: "120px", format: (r) => fmtCount(r.duplicated_groups), selector: (r) => r.duplicated_groups },
    { id: "wasted", name: useKB ? "Wasted (KB)" : "Wasted", right: true, width: useKB ? "120px" : "100px", cell: byteCell(r => r.wasted_bytes, fmtB, useKB), selector: (r) => r.wasted_bytes },
  ];
  return (
    <>
      <h3>Waste by Array Element Type</h3>
      <StdTable columns={cols} data={rows} searchKeys={["array_class"]} fmtBtn={kbBtn} />
    </>
  );
}

function DupArrayHoldersTable({ rows }: { rows: DupArrayHolder[] }) {
  const cols: TableColumn<DupArrayHolder>[] = [
    { id: "rank", name: "#", right: true, width: "52px", cell: (_r, i) => (i ?? 0) + 1 },
    { id: "class", name: "Class", grow: 1, cell: (h) => <code>{h.class_name}</code> },
    { id: "refs", name: "Array refs", right: true, width: "120px", format: (h) => fmtCount(h.array_refs), selector: (h) => h.array_refs },
  ];
  return (
    <>
      <h3>Classes Holding the Most Duplicate Arrays</h3>
      <StdTable columns={cols} data={rows} searchKeys={["class_name"]} />
    </>
  );
}

// ── Duplicate Strings (approximate) ────────────────────────────────────────────
function DuplicateStringsSection({ report }: { report: Report }) {
  const [fmtB] = useFmtBytes();
  const d = report.overview.duplicate_strings;
  if (!d) {
    const wasRun = report.analysis_flags?.find_duplicates ?? false;
    return (
      <section id="duplicate-strings-approximate">
        <h2>Duplicate Strings (approximate)</h2>
        <p className="subtitle">
          {wasRun
            ? "Duplicate-string analysis ran but found no data."
            : <>
                Not run. Re-analyze with the <strong>Full Analysis</strong> option (browser)
                {" "}or pass <code>--find-duplicates</code> (CLI).
              </>
          }
        </p>
      </section>
    );
  }
  const w = d.char_array_waste;
  return (
    <section id="duplicate-strings-approximate">
      <h2>Duplicate Strings (approximate)</h2>
      <p className="subtitle">
        Opt-in (<code>--find-duplicates</code>): each <code>java.lang.String</code> value hashed to 64 bits; collisions accepted as approximation.
      </p>
      <ul>
        <li>Total String instances: {fmtCount(d.total_string_instances)}</li>
        <li>Distinct values: {fmtCount(d.distinct_values)}</li>
        <li>Duplicated values: {fmtCount(d.duplicated_values)}</li>
        <li>Approx wasted bytes: {fmtB(d.approx_wasted_bytes)}</li>
      </ul>

      {d.top_duplicated.length > 0 && (
        <TopDuplicatedTable rows={d.top_duplicated} />
      )}

      {d.top_by_length.length > 0 && (
        <TopByLengthTable rows={d.top_by_length} />
      )}

      {d.length_histogram.length > 0 && (() => {
        type LenBucket = (typeof d.length_histogram)[0];
        const lenCols: TableColumn<LenBucket>[] = [
          { id: "upper", name: "Length ≤", right: true, width: "120px", format: (b) => fmtCount(b.upper_len), selector: (b) => b.upper_len },
          { id: "values", name: "Values", right: true, width: "120px", format: (b) => fmtCount(b.count), selector: (b) => b.count },
        ];
        return (
          <>
            <h3>String Length Distribution</h3>
            <p className="subtitle">
              Distinct-value lengths (bytes): min {fmtCount(d.length_stats.min)}, median {fmtCount(d.length_stats.median)},
              max {fmtCount(d.length_stats.max)}; total {fmtB(d.length_stats.total)}.
            </p>
            <StdTable columns={lenCols} data={d.length_histogram} searchKeys={[]} />
          </>
        );
      })()}

      {d.top_string_holders.length > 0 && (
        <StringHoldersTable rows={d.top_string_holders} />
      )}

      {w && (
        <>
          <h3><code>char[]</code> Waste</h3>
          <p className="subtitle">
            {fmtCount(w.arrays_examined)} arrays examined, {fmtCount(w.wasteful_arrays)} wasteful,{" "}
            {fmtB(w.total_wasted_bytes)} total wasted.
          </p>
          {w.top.length > 0 && (
            <CharArrayWasteTopTable rows={w.top} />
          )}
        </>
      )}
    </section>
  );
}

function DuplicatePrimArraysSection({ report }: { report: Report }) {
  const [fmtB] = useFmtBytes();
  const d = report.overview.duplicate_prim_arrays;
  if (!d) {
    const wasRun = report.analysis_flags?.find_duplicates ?? false;
    return (
      <section id="duplicate-prim-arrays">
        <h2>Duplicate Primitive Arrays (approximate)</h2>
        <p className="subtitle">
          {wasRun
            ? "Duplicate primitive-array analysis ran but found no data."
            : <>
                Not run. Re-analyze with the <strong>Full Analysis</strong> option (browser)
                {" "}or pass <code>--find-duplicates</code> (CLI).
              </>
          }
        </p>
      </section>
    );
  }
  return (
    <section id="duplicate-prim-arrays">
      <h2>Duplicate Primitive Arrays (approximate)</h2>
      <p className="subtitle">
        Opt-in (<code>--find-duplicates</code>): each primitive array hashed to 64 bits by
        content and element type; collisions accepted as approximation.
      </p>
      <ul>
        <li>Approx wasted bytes: {fmtB(d.total_wasted_bytes)}</li>
      </ul>
      {d.rows.length > 0 && (
        <DupPrimArrayRowsTable rows={d.rows} />
      )}
      {d.top_array_holders && d.top_array_holders.length > 0 && (
        <DupArrayHoldersTable rows={d.top_array_holders} />
      )}
    </section>
  );
}

function BoxedNumbersSection({ report }: { report: Report }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const rows = report.overview.boxed_numbers;
  if (!rows?.length) return null;
  const total = report.overview.total_shallow;
  const holders = report.overview.boxed_number_holders ?? [];
  const boxedCols: TableColumn<import("./types").BoxedNumberRow>[] = [
    { id: "rank", name: "#", right: true, width: "52px", cell: (_r, i) => (i ?? 0) + 1 },
    { id: "class", name: "Class", grow: 1, cell: (r) => <span className="copy-cell"><code>{r.pretty_class}</code><CopyBtn text={r.pretty_class} /></span> },
    { id: "instances", name: "Instances", right: true, width: "120px", format: (r) => fmtCount(r.instances), selector: (r) => r.instances },
    { id: "shallow", name: useKB ? "Total Shallow (KB)" : "Total Shallow", right: true, width: useKB ? "150px" : "120px", cell: byteCell(r => r.total_shallow, fmtB, useKB), selector: (r) => r.total_shallow },
    { id: "pct", name: "% of Heap", right: true, width: "100px", format: (r) => total > 0 ? fmtPct(r.pct_of_heap_bp / 100) : "—", selector: (r) => r.pct_of_heap_bp },
    { id: "avg", name: useKB ? "Avg Size (KB)" : "Avg Size", right: true, width: useKB ? "120px" : "100px", cell: byteCell(r => r.avg_shallow, fmtB, useKB), selector: (r) => r.avg_shallow },
  ];
  const holderCols: TableColumn<import("./types").BoxedNumberHolder>[] = [
    { id: "rank", name: "#", right: true, width: "52px", cell: (_r, i) => (i ?? 0) + 1 },
    { id: "class", name: "Class", grow: 1, cell: (h) => <span className="copy-cell"><code>{h.class_name}</code><CopyBtn text={h.class_name} /></span> },
    { id: "refs", name: "Boxed refs", right: true, width: "120px", format: (h) => fmtCount(h.boxed_refs), selector: (h) => h.boxed_refs },
  ];
  return (
    <section id="boxed-numbers">
      <h2>Boxed Numbers</h2>
      <p className="subtitle">
        Wrapper types whose instances occupy heap that could be replaced with primitives.
      </p>
      <StdTable columns={boxedCols} data={rows} searchKeys={["pretty_class"]} fmtBtn={kbBtn} />
      {holders.length > 0 && (
        <>
          <h3>Classes Holding the Most Boxed-Number References</h3>
          <StdTable columns={holderCols} data={holders} searchKeys={["class_name"]} />
        </>
      )}
    </section>
  );
}

function HeaderOverheadSection({ report }: { report: Report }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const rows = report.overview.header_overhead;
  if (!rows?.length) return null;
  const cols: TableColumn<import("./types").HeaderOverheadRow>[] = [
    { id: "rank", name: "#", right: true, width: "52px", cell: (_r, i) => (i ?? 0) + 1 },
    { id: "class", name: "Class", grow: 1, cell: (r) => <span className="copy-cell"><code>{r.pretty_class}</code><CopyBtn text={r.pretty_class} /></span> },
    { id: "instances", name: "Instances", right: true, width: "120px", format: (r) => fmtCount(r.instances), selector: (r) => r.instances },
    { id: "hdr", name: "Hdr/obj", right: true, width: "90px", format: (r) => `${r.header_bytes} B`, selector: (r) => r.header_bytes },
    { id: "total_hdr", name: useKB ? "Total Headers (KB)" : "Total Headers", right: true, width: useKB ? "150px" : "130px", cell: byteCell(r => r.total_header_bytes, fmtB, useKB), selector: (r) => r.total_header_bytes },
    { id: "pct", name: "Hdr %", right: true, width: "90px", format: (r) => fmtPct(r.header_pct_of_shallow_bp / 100), selector: (r) => r.header_pct_of_shallow_bp },
    { id: "avg", name: useKB ? "Avg Size (KB)" : "Avg Size", right: true, width: useKB ? "120px" : "100px", cell: byteCell(r => r.avg_shallow, fmtB, useKB), selector: (r) => r.avg_shallow },
  ];
  return (
    <section id="object-header-overhead">
      <h2>Object Header Overhead</h2>
      <p className="subtitle">
        Classes where object headers consume a large share of shallow heap
        (candidates for value-type / record optimisation).
      </p>
      <StdTable columns={cols} data={rows} searchKeys={["pretty_class"]} fmtBtn={kbBtn} />
    </section>
  );
}
function SystemOverviewSection({ report }: { report: Report }) {
  const fmtB = formatBytes;
  const o = report.overview;
  const threadCount = report.threads?.threads?.length ?? 0;
  return (
    <section id="system-overview">
      <h2>System Overview</h2>
      <p className="subtitle">Reachable-heap totals and the largest classes by retained heap.</p>

      <div className="card">
        <dl className="summary-grid">
          <dt>Source file</dt>
          <dd>
            <code title={o.file_path}>{o.source_name}</code>
            {o.file_path && o.file_path !== o.source_name && (
              <span className="hint" style={{ display: "block" }}>
                {o.file_path}
              </span>
            )}
          </dd>
          <dt>HPROF format</dt>
          <dd>{o.format}</dd>
          {o.jvm_version && (
            <>
              <dt>JVM version</dt>
              <dd>
                <code>{o.jvm_version}</code>
              </dd>
            </>
          )}
          <dt>File size</dt>
          <dd>{fmtB(o.file_size)}</dd>
          <dt>Identifier size</dt>
          <dd>{o.identifier_size_bits}-bit</dd>
          {o.compressed_oops !== null && (
            <>
              <dt>Compressed OOPs</dt>
              <dd>{o.compressed_oops ? "yes" : "no"}</dd>
            </>
          )}
          {o.dump_creation !== null && (
            <>
              <dt>Dump created</dt>
              <dd>{formatEpochMs(o.dump_creation)}</dd>
            </>
          )}
          <dt>Total objects</dt>
          <dd>{fmtCount(o.total_objects)}</dd>
          <dt>Total reachable heap</dt>
          <dd>{fmtB(o.total_shallow)}</dd>
          <dt>GC roots</dt>
          <dd>{fmtCount(o.gc_roots)}</dd>
          <dt>Classes loaded</dt>
          <dd>{fmtCount(o.classes_loaded)}</dd>
          <dt>Class loaders</dt>
          <dd>{fmtCount(o.classloaders_loaded)}</dd>
          {threadCount > 0 && (
            <>
              <dt>Threads (with call stacks)</dt>
              <dd>
                <a href="#threads">{fmtCount(threadCount)}</a>
              </dd>
            </>
          )}
          {o.unreachable_count > 0 && (
            <>
              <dt>Unreachable (excluded)</dt>
              <dd>
                {fmtCount(o.unreachable_count)} ({fmtB(o.unreachable_shallow)})
              </dd>
            </>
          )}
          {(o.heap_fragmentation_ratio ?? 0) > 0 && (
            <>
              <dt>Heap fragmentation (unreachable / heap total)</dt>
              <dd>{fmtPct((o.heap_fragmentation_ratio ?? 0) * 100)}</dd>
            </>
          )}
          {(o.top_class_concentration_bp ?? 0) > 0 && (
            <>
              <dt>Top-class retained concentration</dt>
              <dd>{fmtPct((o.top_class_concentration_bp ?? 0) / 100)}</dd>
            </>
          )}
        </dl>
      </div>

      {o.system_properties.length > 0 ? (
        <details>
          <summary>System properties ({fmtCount(o.system_properties.length)})</summary>
          <SysPropsTable rows={o.system_properties} />
        </details>
      ) : (
        <p className="subtitle">System properties not captured in this dump.</p>
      )}

      {o.heap_composition.by_kind.length > 0 && (() => {
        const compCols: TableColumn<KindStat>[] = [
          { id: "kind", name: "Kind", grow: 1, selector: (k) => k.kind },
          { id: "objects", name: "Objects", right: true, width: "120px", format: (k) => fmtCount(k.objects), selector: (k) => k.objects },
          { id: "shallow", name: "Shallow Heap", right: true, width: "120px", format: (k) => fmtB(k.shallow_heap), selector: (k) => k.shallow_heap },
        ];
        return (
          <>
            <h3>Heap Composition</h3>
            <ChartOrNote hasData={o.heap_composition.by_kind.length >= 2} note="Composition chart needs at least two kinds; showing the table only.">
              <HeapCompositionChart data={o.heap_composition.by_kind} />
              <CompositionStackedBar data={o.heap_composition.by_kind} />
            </ChartOrNote>
            <StdTable columns={compCols} data={o.heap_composition.by_kind} searchKeys={["kind"]} />
          </>
        );
      })()}

      {(o.gc_roots_retained_by_type?.length ?? o.gc_roots_by_type.length) > 0 && (() => {
        const gcRows = o.gc_roots_retained_by_type?.length
          ? o.gc_roots_retained_by_type
          : o.gc_roots_by_type.map((r) => ({ ...r, retained: 0 }));
        const maxCount = Math.max(...gcRows.map((r) => r.count), 1);
        const totalCount = gcRows.reduce((s, r) => s + r.count, 0);
        const totalRetained = gcRows.reduce((s, r) => s + r.retained, 0);
        type GcRow = (typeof gcRows)[0];
        const gcCols: TableColumn<GcRow>[] = [
          {
            id: "bar", name: "", width: "90px", grow: 0,
            cell: (r) => (
              <span className="bar-bg" style={{ width: 80 }}>
                <span className="bar-fill" style={{ width: `${(r.count / maxCount) * 100}%` }} />
              </span>
            ),
          },
          { id: "type", name: "Root Type", grow: 1, selector: (r) => r.root_type },
          { id: "count", name: "Count", right: true, width: "100px", format: (r) => fmtCount(r.count), selector: (r) => r.count },
          { id: "pct", name: "%", right: true, width: "80px", format: (r) => fmtPct(totalCount > 0 ? (r.count / totalCount) * 100 : 0), selector: (r) => r.count },
          { id: "retained", name: "Retained", right: true, width: "120px", format: (r: GcRow) => fmtB(r.retained), selector: (r: GcRow) => r.retained },
        ];
        return (
          <>
            <h3>GC Roots by Type</h3>
            <StdTable columns={gcCols} data={gcRows} searchKeys={["root_type"]} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ width: "90px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }} />
              <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(totalCount)}</span>
              <span style={{ width: "80px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>100%</span>
              <span style={{ width: "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalRetained)}</span>
            </div>
          </>
        );
      })()}

      <h3>Class Histogram (by Retained Heap)</h3>
      {o.histogram_truncated_to != null && (
        <p className="subtitle">
          Histogram capped to the largest {fmtCount(o.histogram_truncated_to)} classes.
        </p>
      )}
      <ChartOrNote hasData={o.histogram.length > 0} note="No histogram classes to chart.">
        <TopClassesChart data={o.histogram} />
      </ChartOrNote>
      <ClassHistogramTable rows={o.histogram} totalShallow={o.total_shallow} />

      {o.loader_rollup.length > 0 && (
        <>
          <h3>Class Loaders</h3>
          <p className="subtitle">
            Classes grouped by the loader that defined them. Many loaders each holding heap — especially the same class
            name under several loaders — can signal a class-loader leak.
          </p>
          <LoaderRollupChart data={o.loader_rollup} />
          <ClassLoadersTable rows={o.loader_rollup} />
        </>
      )}

      {o.duplicate_classes.length > 0 && (
        <>
          <h3>Duplicate Classes</h3>
          <p className="subtitle">
            Class names loaded by more than one class loader — a classic class-loader-leak signature (the same class
            re-loaded repeatedly, e.g. per web-app or plugin reload).
          </p>
          <DuplicateClassesTable rows={o.duplicate_classes} />
        </>
      )}
    </section>
  );
}

// ── Leak Suspects ───────────────────────────────────────────────────────────
function ClassLoadersTable({ rows }: { rows: LoaderRollup[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const cols: TableColumn<LoaderRollup>[] = [
    { id: "loader", name: "Loader", grow: 1, cell: (r) => <code title={r.loader_label ?? undefined}>{r.loader_label ? fmtLoader(r.loader_label) : `loader@${r.loader_id}`}</code> },
    { id: "classes", name: "Classes", right: true, width: "90px", format: (r) => fmtCount(r.class_count), selector: (r) => r.class_count },
    { id: "instances", name: "Instances", right: true, width: "120px", format: (r) => fmtCount(r.instances), selector: (r) => r.instances },
    { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.shallow, fmtB, useKB), selector: (r) => r.shallow },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.retained, fmtB, useKB), selector: (r) => r.retained },
  ];
  return <StdTable columns={cols} data={rows} searchKeys={["loader_label"]} fmtBtn={kbBtn} />;
}

function DuplicateClassesTable({ rows }: { rows: DuplicateClass[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const loaderDetailCols: TableColumn<typeof rows[0]["per_loader"][0]>[] = [
    { id: "loader", name: "Loader", grow: 1,
      cell: pl => <code title={pl.loader_label ?? undefined}>{pl.loader_label ? fmtLoader(pl.loader_label) : "—"}</code>,
      selector: pl => pl.loader_label ?? "" },
    { id: "instances", name: "Instances", right: true, width: "100px",
      format: pl => fmtCount(pl.instances), selector: pl => pl.instances },
    { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "120px" : "100px",
      cell: byteCell(pl => pl.shallow, fmtB, useKB), selector: pl => pl.shallow },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "120px" : "100px",
      cell: byteCell(pl => pl.retained, fmtB, useKB), selector: pl => pl.retained },
  ];
  const cols: TableColumn<DuplicateClass>[] = [
    {
      id: "class", name: "Class", grow: 1,
      cell: (d) => (
        <span title={d.loaders.join(", ")}>
          {d.per_loader && d.per_loader.length > 0 ? (
            <details>
              <summary>
                <code>{d.pretty_class}</code>
              </summary>
              <DataTable columns={loaderDetailCols} data={d.per_loader} customStyles={histogramTableStyles} dense />
            </details>
          ) : (
            <code>{d.pretty_class}</code>
          )}
        </span>
      ),
    },
    { id: "loaders", name: "#Loaders", right: true, width: "90px", format: (d) => fmtCount(d.loader_count), selector: (d) => d.loader_count },
    { id: "instances", name: "Instances", right: true, width: "120px", format: (d) => fmtCount(d.total_instances), selector: (d) => d.total_instances },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(d => d.total_retained, fmtB, useKB), selector: (d) => d.total_retained },
  ];
  return <StdTable columns={cols} data={rows} searchKeys={["pretty_class"]} fmtBtn={kbBtn} />;
}

// Renders the accumulation "shortest path" (MAT's signature view) plus the
// per-class breakdown of what piles up at the accumulation point.
function AccumulationPath({ s }: { s: Suspect }) {
  const [fmtB] = useFmtBytes();
  if (s.path.length === 0) return null;
  return (
    <details open>
      <summary>Shortest path to the accumulation point ({s.path.length} steps)</summary>
      <ol className="accum-path">
        {s.path.map((p, i) => (
          <li key={i}>
            <code>{p.display_class}</code>{" "}
            <span className="path-ret">retains {fmtB(p.retained)}</span>
          </li>
        ))}
      </ol>
    </details>
  );
}

function DominatedByClass({ rows, suspectRetained }: { rows: HistRow[]; suspectRetained: number }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  if (rows.length === 0) return null;
  const cols: TableColumn<HistRow>[] = [
    { id: "class", name: "Class", grow: 1, cell: (r) => <code>{r.pretty_class}</code> },
    { id: "instances", name: "Instances", right: true, width: "120px", format: (r) => fmtCount(r.instances), selector: (r) => r.instances },
    { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.shallow, fmtB, useKB), selector: (r) => r.shallow },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.retained, fmtB, useKB), selector: (r) => r.retained },
    { id: "pct", name: "% of suspect", right: true, width: "120px", format: (r) => suspectRetained > 0 ? fmtPct(pctOf(r.retained, suspectRetained)) : "—", selector: (r) => r.retained },
  ];
  return (
    <details open>
      <summary>Accumulated objects by class ({rows.length})</summary>
      <StdTable columns={cols} data={rows} searchKeys={["pretty_class"]} fmtBtn={kbBtn} />
    </details>
  );
}

function SysPropsTable({ rows }: { rows: { key: string; value: string }[] }) {
  const sysCols: TableColumn<{ key: string; value: string }>[] = [
    { id: "key", name: "Key", width: "280px", grow: 0,
      cell: r => <code>{r.key}</code>, selector: r => r.key, sortable: true },
    { id: "val", name: "Value", grow: 1,
      cell: r => <ExpandableText text={r.value} label={`${r.key}`} />,
      selector: r => r.value },
  ];
  return <StdTable columns={sysCols} data={rows} searchKeys={["key", "value"]} />;
}

// the dominator chain from a suspect
// (first) up to its GC root (last), as a numbered list. The final step is annotated
// with the GC-root type when known. Mirrors report.rs::render_root_path.
function RootPathList({ steps }: { steps: RootPathStep[] }) {
  const [fmtB] = useFmtBytes();
  if (steps.length === 0) return null;
  const last = steps.length - 1;
  return (
    <details>
      <summary>Dominator chain to GC root ({steps.length} step{steps.length === 1 ? "" : "s"})</summary>
      <ol className="accum-path">
        {steps.map((p, i) => (
          <li key={i}>
            {p.field_edge && <span className="path-field">.{p.field_edge} → </span>}
            <code>{p.display_class}</code>{" "}
            <span className="path-ret">retains {fmtB(p.retained)}</span>
            {i === last && p.root_type_label && (
              <> — <strong>GC root: {p.root_type_label}</strong></>
            )}
          </li>
        ))}
      </ol>
    </details>
  );
}


// One node of the recursive "merged shortest paths to GC roots" prefix tree
// (class-group suspects). Mirrors DomSubtreeNode. Each node shows the class, how
// many member chains pass through it, and the aggregate retained heap; a
// terminal GC-root node carries its root-type label.
function MergedPathsNode({ node, depth }: { node: MergedPathNode; depth: number }) {
  const [fmtB] = useFmtBytes();
  const hasChildren = node.children.length > 0;
  const label = (
    <>
      {node.field_edge && <span className="path-field">.{node.field_edge} → </span>}
      <code>{node.display_class}</code>{" "}
      <span className="path-ret">
        {fmtCount(node.object_count)} object{node.object_count === 1 ? "" : "s"} · retained {fmtB(node.retained)}
      </span>
      {node.root_type_label && (
        <> — <strong>GC root: {node.root_type_label}</strong></>
      )}
    </>
  );
  if (!hasChildren) {
    return (
      <li style={{ paddingLeft: `${depth * 1.1}rem` }}>
        <span className="tree-leaf">•</span> {label}
      </li>
    );
  }
  return (
    <li>
      <details open={depth < 1}>
        <summary style={{ paddingLeft: `${depth * 1.1}rem` }}>{label}</summary>
        <ul className="dom-subtree">
          {node.children.map((c, i) => (
            <MergedPathsNode key={i} node={c} depth={depth + 1} />
          ))}
        </ul>
      </details>
    </li>
  );
}

function MergedPaths({ node }: { node: MergedPathNode }) {
  return (
    <details>
      <summary>Merged paths to GC roots</summary>
      <ul className="dom-subtree">
        <MergedPathsNode node={node} depth={0} />
      </ul>
    </details>
  );
}

function SuspectCard({ s, total, rank }: { s: Suspect; total: number; rank: number }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const share = pctOf(s.retained, total);
  return (
    <div className="suspect" id={`suspect-${rank}`}>
      <h3 style={{ margin: "0 0 0.25rem" }}>
        <span className="rank">Suspect #{rank}</span> <code>{s.pretty_class}</code>
        <span className="pill">{s.is_single ? "single object" : `class group ×${fmtCount(s.instance_count)}`}</span>
      </h3>
      <p style={{ margin: "0.25rem 0" }}>
        Retains <strong title={fmtExactBytes(s.retained)}>{fmtB(s.retained)}</strong>{" "}
        <span className="mat-exact">
          {fmtExactBytes(s.retained)} ({fmtPct(share)})
        </span>
        {s.shallow > 0 && <> · shallow {fmtB(s.shallow)}</>}.
      </p>
      <p style={{ margin: "0.25rem 0" }}>
        <span className="label">Held by:</span>{" "}
        {s.root_type_label ? (
          <>
            a <strong>{s.root_type_label}</strong> GC root
          </>
        ) : (
          <span style={{ color: "var(--muted)" }}>multiple / ambiguous roots (no single holding root identified)</span>
        )}
      </p>
      {s.keywords.length > 0 && (
        <p style={{ margin: "0.25rem 0" }}>
          <span className="label">Keywords:</span>{" "}
          {s.keywords.map((k, i) => (
            <span key={i} className="pill keyword" title="Class involved in this suspect">
              {k}
            </span>
          ))}
        </p>
      )}
      {s.accumulation_class && (
        <p style={{ margin: "0.25rem 0", color: "var(--muted)", fontSize: "0.86rem" }}>
          Accumulation point: <code>{s.accumulation_class}</code>
          {s.accumulation_retained != null && <> retaining {fmtB(s.accumulation_retained)}</>}.
        </p>
      )}
      <DominatedByClass rows={s.dominated_by_class} suspectRetained={s.retained} />
      <AccumulationPath s={s} />
      {s.dominated.length > 0 && (() => {
        const domCols: TableColumn<import("./types").DominatedRow>[] = [
          { id: "class", name: "Class", grow: 1, cell: (d) => <code>{d.display_class}</code> },
          { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: byteCell(d => d.shallow, fmtB, useKB), selector: (d) => d.shallow },
          { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(d => d.retained, fmtB, useKB), selector: (d) => d.retained },
        ];
        return (
          <details>
            <summary>
              Accumulated objects in dominator tree{" "}
              {s.dominated_total_count > s.dominated_shown
                ? `(directly dominates ${fmtCount(s.dominated_total_count)}, showing top ${fmtCount(s.dominated_shown)})`
                : `(directly dominates ${fmtCount(s.dominated_total_count)})`}
            </summary>
            <StdTable columns={domCols} data={s.dominated} searchKeys={["display_class"]} fmtBtn={kbBtn} />
          </details>
        );
      })()}
      {s.root_path && <RootPathList steps={s.root_path} />}
      {s.dominator_tree && <DomSubtreeSvg node={s.dominator_tree} />}
      {!s.is_single && s.merged_paths && <MergedPaths node={s.merged_paths} />}
    </div>
  );
}

function LeakSuspectsSection({ report }: { report: Report }) {
  const l = report.leaks;
  return (
    <section id="leak-suspects">
      <h2>Leak Suspects</h2>
      <p className="subtitle">Ranked accumulation points holding the most retained heap.</p>
      {l.suspects.length === 0 ? (
        <p>No suspect exceeds the leak threshold; retention is spread across many roots.</p>
      ) : (
        <>
          <h3>Overview — retained-heap share</h3>
          <p className="subtitle">
            How concentrated the leak is: each slice is one suspect&apos;s retained heap; the remainder is everything
            else on the reachable heap.
          </p>
          <ChartOrNote hasData={l.suspects.length > 0 && l.total_shallow > 0} note="No leak suspects to chart.">
            <LeakShareChart suspects={l.suspects} total={l.total_shallow} onSlice={(i) => {
              if (i < l.suspects.length)
                document.getElementById(`suspect-${i + 1}`)?.scrollIntoView({ behavior: "smooth", block: "center" });
            }} />
          </ChartOrNote>
          {l.suspects.map((s, i) => (
            <SuspectCard key={i} s={s} total={l.total_shallow} rank={i + 1} />
          ))}
        </>
      )}
    </section>
  );
}

// ── Top Consumers ───────────────────────────────────────────────────────────
// A recursive, expandable package tree (MAT PackageTreeResult drill-down). Each
// node shows cumulative # objects / shallow / retained over its subtree.
function PackageTreeRow({ node, depth, maxRetained, rowId }: { node: PackageNode; depth: number; maxRetained: number; rowId?: string }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const [open, setOpen] = React.useState(depth < 1);
  const hasChildren = node.children.length > 0;
  const label = node.name || "(default package)";
  const pct = maxRetained > 0 ? (node.retained_heap / maxRetained) * 100 : 0;
  return (
    <>
      <tr id={rowId}>
        <td>
          <span style={{ paddingLeft: `${depth * 1.1}rem` }}>
            {hasChildren ? (
              <button className="tree-toggle" onClick={() => setOpen(!open)} aria-expanded={open}>
                {open ? "▾" : "▸"}
              </button>
            ) : (
              <span className="tree-leaf">•</span>
            )}
            <code>{label}</code>
          </span>
        </td>
        <td className="num">{fmtCount(node.top_dominator_count)}</td>
        <td className="num">{fmtB(node.shallow_heap)}</td>
        <td className="num bar-cell">
          <span className="bar-bg">
            <span className="bar-fill" style={{ width: `${pct}%` }} />
          </span>
          {fmtB(node.retained_heap)}
        </td>
      </tr>
      {open &&
        node.children.map((c, i) => (
          <PackageTreeRow key={i} node={c} depth={depth + 1} maxRetained={maxRetained} />
        ))}
    </>
  );
}

function TopConsumersSection({ report }: { report: Report }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const [fmtBcls, kbBtnCls, useKBcls] = useFmtBytes();
  const t = report.top;
  const total = report.leaks.total_shallow;
  const pkgRoot = t.biggest_packages;
  const maxPkgRetained = pkgRoot.children.reduce((m, c) => Math.max(m, c.retained_heap), 0);

  const objHasOwner = t.biggest_objects.some((o) => !!o.owner || !!o.held_via);

  const objTableCols: TableColumn<ObjRow>[] = [
    { id: "rank", name: "#", right: true, width: "52px", cell: (_r, i) => (i ?? 0) + 1 },
    { id: "class", name: "Class", grow: 1, cell: (o) => <code title={o.display_class}>{o.display_class}</code> },
    { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: byteCell(o => o.shallow, fmtB, useKB), selector: (o) => o.shallow, sortable: true },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: (o) => <span title={fmtExactBytes(o.retained)}>{fmtB(o.retained)}</span>, selector: (o) => o.retained, sortable: true },
    { id: "pct", name: "% Heap", right: true, width: "100px", format: (o) => fmtPct(pctOf(o.retained, total)), selector: (o) => o.pct_bp, sortable: true },
    ...(objHasOwner ? [{ id: "held_via", name: "Held via (Class#field)", grow: 1, minWidth: "160px", cell: (o: ObjRow) => o.owner ? <ExpandableText text={o.owner} label="Held via" /> : o.held_via ? <><ExpandableText text={o.held_via} label="Held via" /> <span className="muted">(stack)</span></> : <span>—</span> } as TableColumn<ObjRow>] : []),
  ];

  const clsTableCols: TableColumn<ClassRow>[] = [
    { id: "class", name: "Class", grow: 1, cell: (c) => <span className="copy-cell"><code title={c.pretty_class}>{c.pretty_class}</code><CopyBtn text={c.pretty_class} /></span> },
    { id: "instances", name: "Instances", right: true, width: "120px", format: (c) => fmtCount(c.instances), selector: (c) => c.instances, sortable: true },
    { id: "retained", name: useKBcls ? "Retained (KB)" : "Retained", right: true, width: useKBcls ? "130px" : "110px", cell: (c) => <span title={fmtExactBytes(c.retained)}>{fmtBcls(c.retained)}</span>, selector: (c) => c.retained, sortable: true },
    { id: "pct", name: "% Heap", right: true, width: "100px", format: (c) => fmtPct(pctOf(c.retained, total)), selector: (c) => c.retained },
  ];

  return (
    <section id="top-consumers">
      <h2>Top Consumers</h2>
      <p className="subtitle">Biggest individual objects, classes, and packages by retained heap.</p>

      <h3>Biggest Objects</h3>
      {objHasOwner && (
        <p className="subtitle">
          The <strong>Held via</strong> column names the dominant incoming <code>Class#field</code>{" "}
          reference that holds each object (the primary referrer; an object may have several).
        </p>
      )}
      <StdTable columns={objTableCols} data={t.biggest_objects} searchKeys={["display_class"]} fmtBtn={kbBtn} defaultSortFieldId="retained" />

      <h3>Biggest Classes</h3>
      <StdTable columns={clsTableCols} data={t.biggest_classes} searchKeys={["pretty_class"]} fmtBtn={kbBtnCls} defaultSortFieldId="retained" />

      {pkgRoot.children.length > 0 && (
        <>
          <h3>Biggest Packages</h3>
          <p className="subtitle">
            Expand a package to drill into its sub-packages. Totals are cumulative over the subtree. Only top-level
            dominators retaining at least {fmtPct(t.threshold_bp / 100)} of the
            heap are included (smaller ones are pruned, MAT-style).
          </p>
          <TreemapBar
            root={pkgRoot}
            onSelect={(idx) => document.getElementById(`pkg-${idx}`)?.scrollIntoView({ behavior: "smooth", block: "center" })}
          />
          <details style={{ marginBottom: "1rem" }}>
            <summary style={{ cursor: "pointer", userSelect: "none" }}>Retained-heap treemap</summary>
            <div style={{ marginTop: "0.5rem", overflowX: "auto" }}>
              <RetainedTreemap root={pkgRoot} />
            </div>
          </details>
          <table className="tree-table">
            <thead>
              <tr>
                <th>Package</th>
                <th className="num"># Objects</th>
                <th className="num">Shallow</th>
                <th className="num">Retained</th>
              </tr>
            </thead>
            <tbody>
              {pkgRoot.children.map((p, i) => (
                <PackageTreeRow key={i} node={p} depth={0} maxRetained={maxPkgRetained} rowId={`pkg-${i}`} />
              ))}
            </tbody>
          </table>
        </>
      )}
    </section>
  );
}

// ── Threads ─────────────────────────────────────────────────────────────────
// One collapsible block per thread; frames rendered verbatim in a monospace
// <pre>. A filter box keeps large thread sets (hundreds) navigable. Preserves
// the upstream (thread_serial-sorted) order for determinism.
// a small table of a thread's GC-thread-local root
// objects. Renders nothing for an empty list. Mirrors report.rs::render_thread_locals.
function ThreadLocalsTable({ objs, totalCount }: { objs: ThreadLocalObj[]; totalCount: number }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  if (objs.length === 0) return null;
  const cols: TableColumn<ThreadLocalObj>[] = [
    { id: "obj", name: "Object", grow: 1, cell: (o) => <span className="copy-cell"><code>{o.display_class}</code><CopyBtn text={o.display_class} /></span> },
    { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: byteCell(o => o.shallow, fmtB, useKB), selector: (o) => o.shallow },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(o => o.retained, fmtB, useKB), selector: (o) => o.retained },
  ];
  return (
    <div className="thread-locals-inline">
      <p className="thread-locals-label">Local root objects ({fmtCount(objs.length)}
        {objs.length < totalCount && ` — showing top ${fmtCount(objs.length)} of ${fmtCount(totalCount)}; sizes overlap and do not sum to thread total`}
      )</p>
      <StdTable columns={cols} data={objs} searchKeys={["display_class"]} fmtBtn={kbBtn} />
    </div>
  );
}

function threadStateLabel(raw: string): string {
  // "alive, waiting, waiting indefinitely, parked" → "waiting"
  const parts = raw.replace(/[\[\]]/g, "").split(",").map((s) => s.trim()).filter(Boolean);
  const nonAlive = parts.filter((p) => p !== "alive" && p !== "runnable");
  return nonAlive[nonAlive.length - 1] ?? parts[0] ?? raw;
}

function fmtLoader(raw: string): string {
  // "org/foo/Bar @ 0xdeadbeef" → "org.foo.Bar"
  return raw.replace(/\s*@\s*0x[0-9a-fA-F]+$/, "").replaceAll("/", ".");
}

function ThreadCard({ t, open }: { t: ThreadInfo; open?: boolean }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const name = t.name?.trim();
  const cls = (t.class_name ?? "<unresolved>").replaceAll("/", ".");
  const sig = t.significant_frames ?? [];
  const stateLabel = t.thread_state ? threadStateLabel(t.thread_state) : null;
  return (
    <details className="thread" open={open} id={`thread-${t.thread_serial}`}>
      <summary>
        <span className="thread-name">{name ? `"${name}"` : `Thread ${t.thread_serial}`}</span>
        {name && <span className="thread-serial"> · Thread {t.thread_serial}</span>}
        {" "}<span className="thread-meta-inline">
          {fmtB(t.retained)} retained
          {stateLabel && <span className="thread-state-badge">{stateLabel}</span>}
          {t.is_daemon && <span className="thread-daemon-badge">daemon</span>}
        </span>
      </summary>
      <div className="thread-body">
        <div className="thread-meta-row">
          <span className="thread-meta-item"><span className="thread-meta-label">class</span><code>{cls}</code></span>
          <span className="thread-meta-item"><span className="thread-meta-label">shallow</span>{fmtB(t.shallow)}</span>
          <span className="thread-meta-item"><span className="thread-meta-label">retained</span>{fmtB(t.retained)}</span>
          <span className="thread-meta-item"><span className="thread-meta-label">max local retained</span>{fmtB(t.max_local_retained)}</span>
          <span className="thread-meta-item"><span className="thread-meta-label">priority</span>{t.priority}</span>
          {t.context_class_loader && (
            <span className="thread-meta-item"><span className="thread-meta-label">loader</span><code>{fmtLoader(t.context_class_loader)}</code></span>
          )}
          {t.thread_state && (
            <span className="thread-meta-item"><span className="thread-meta-label">state</span>{t.thread_state.replace(/[\[\]]/g, "")}</span>
          )}
        </div>
        {t.local_objects && <ThreadLocalsTable objs={t.local_objects} totalCount={t.local_root_count} />}
        {sig.length > 0 ? (
          <>
            <p className="subtitle"><em>Frame percentages are of this thread's {fmtB(t.retained)} retained heap.</em></p>
          <ul className="sig-frames">
            {sig.map((sf, i) => (
              <li key={i}>
                <code>{sf.frame}</code>
                {sf.locals.length > 0 && (
                  <ul>
                    {sf.locals.map((loc, j) => (
                      <li key={j}>
                        <code>{loc.display_class}</code>{" "}
                        <span className="path-ret">retains {fmtB(loc.retained)} ({fmtPct(loc.pct)} of thread retained)</span>
                      </li>
                    ))}
                  </ul>
                )}
              </li>
            ))}
          </ul>
          </>
        ) : (
          <pre className="stack">{t.frames.join("\n")}</pre>
        )}
      </div>
    </details>
  );
}

// ── Thread Overview table (always-on properties, mirrors MAT columns) ──────────
function ThreadOverviewTable({ threads }: { threads: ThreadInfo[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  if (threads.length === 0) return null;
  const cols: TableColumn<ThreadInfo>[] = [
    { id: "name", name: "Name", grow: 1, minWidth: "120px", cell: (t) => <a href={`#thread-${t.thread_serial}`}>{t.name?.trim() || `<thread ${t.thread_serial}>`}</a> },
    { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "90px", cell: byteCell(t => t.shallow, fmtB, useKB), selector: (t) => t.shallow },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "90px", cell: byteCell(t => t.retained, fmtB, useKB), selector: (t) => t.retained },
    { id: "max_local", name: useKB ? "Max. Locals' Retained (KB)" : "Max. Locals' Retained", right: true, width: useKB ? "200px" : "172px", cell: byteCell(t => t.max_local_retained, fmtB, useKB), selector: (t) => t.max_local_retained },
    { id: "loader", name: "Context Class Loader", grow: 1, minWidth: "145px", cell: (t) => t.context_class_loader ? <code>{fmtLoader(t.context_class_loader)}</code> : <span>—</span> },
    { id: "daemon", name: "Daemon", width: "85px", selector: (t) => t.is_daemon ? 1 : 0, format: (t) => t.is_daemon ? "yes" : "no" },
    { id: "priority", name: "Priority", right: true, width: "80px", format: (t) => String(t.priority), selector: (t) => t.priority },
    { id: "state", name: "State", width: "145px", selector: (t) => t.thread_state ?? "", format: (t) => t.thread_state || "—" },
  ];
  return (
    <details className="thread-overview-detail">
      <summary>Thread Overview ({fmtCount(threads.length)})</summary>
      <StdTable columns={cols} data={threads} searchKeys={["name"]} fmtBtn={kbBtn} />
    </details>
  );
}

function ThreadsSection({ report }: { report: Report }) {
  const CAP = 100;
  const threads = report.threads?.threads ?? [];
  const [filter, setFilter] = React.useState("");
  const [showAll, setShowAll] = React.useState(false);
  const [openAll, setOpenAll] = React.useState<boolean | undefined>(undefined);
  const [genKey, setGenKey] = React.useState(0);
  const view = React.useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return threads;
    return threads.filter(
      (t) =>
        (t.name ?? "").toLowerCase().includes(needle) ||
        (t.class_name ?? "").toLowerCase().includes(needle) ||
        String(t.thread_serial).includes(needle) ||
        t.frames.some((f) => f.toLowerCase().includes(needle)),
    );
  }, [threads, filter]);
  const isFiltering = filter.trim().length > 0;
  const visible = isFiltering || showAll ? view : view.slice(0, CAP);
  return (
    <section id="threads">
      <h2>Threads</h2>
      <p className="subtitle">Per-thread call stacks recorded in the dump.</p>
      {threads.length === 0 ? (
        <p>No thread call stacks were recorded in this dump.</p>
      ) : (
        <>
          <ThreadOverviewTable threads={threads} />
          <div className="tools">
            <input
              type="text"
              className="filter"
              placeholder="Filter threads (name, class, serial, or stack frame)…"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              aria-label="Filter threads"
            />
            <span className="hint">
              {fmtCount(view.length)} of {fmtCount(threads.length)} thread{threads.length === 1 ? "" : "s"}
            </span>
            <button
              className="theme-toggle"
              onClick={() => { setOpenAll(true); setGenKey((k) => k + 1); }}
            >
              Expand all
            </button>
            <button
              className="theme-toggle"
              onClick={() => { setOpenAll(false); setGenKey((k) => k + 1); }}
            >
              Collapse all
            </button>
          </div>
          {visible.map((t, i) => (
            <ThreadCard key={`${genKey}-${i}`} t={t} open={openAll} />
          ))}
          {!isFiltering && !showAll && view.length > CAP && (
            <button
              className="theme-toggle"
              style={{ marginTop: "0.5rem" }}
              onClick={() => setShowAll(true)}
            >
              Show {fmtCount(view.length - CAP)} more threads
            </button>
          )}
        </>
      )}
    </section>
  );
}

// ── Top Components ─────────────────────────────────────────────────────────────
// Retained heap grouped by class loader (component), mirroring Eclipse MAT's
// Top Components view. Mirrors render_md.rs::render_top_components.
type ComponentKey = "retained" | "pct";
const COMPONENT_COLS: { key: ComponentKey; label: string }[] = [
  { key: "retained", label: "Retained" },
  { key: "pct", label: "% Heap" },
];

function TopComponentsSection({ data }: { data: TopComponents }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const components = data?.components ?? [];
  if (components.length === 0) return null;
  const cols: TableColumn<Component>[] = [
    { id: "component", name: "Component", grow: 1, cell: (c) => <code title={c.loader_label ?? undefined}>{fmtLoader(c.loader_label ?? "")}</code> },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: "140px", sortable: true, cell: byteCell(c => c.retained, fmtB, useKB), selector: (c) => c.retained },
    { id: "pct", name: "% Heap", right: true, width: "100px", sortable: true, format: (c) => fmtPct(c.pct), selector: (c) => c.pct },
    {
      id: "top_classes", name: "Top classes", grow: 2,
      cell: (c) => (
        <>
          {c.top_classes.map((cc, j) => (
            <span key={j}>
              {j > 0 ? ", " : ""}
              <code>{cc.pretty_class}</code> ({fmtB(cc.retained)})
            </span>
          ))}
        </>
      ),
    },
  ];
  return (
    <section id="top-components">
      <h2>Top Components</h2>
      <p className="subtitle">
        Retained heap grouped by class loader (component); % Heap is the share of total reachable heap.
      </p>
      <details open>
        <summary>Components by retained heap ({fmtCount(components.length)} rows)</summary>
        <StdTable columns={cols} data={components} searchKeys={["loader_label"]} fmtBtn={kbBtn} defaultSortFieldId="retained" />
      </details>
    </section>
  );
}

// ── Arrays by Size ─────────────────────────────────────────────────────────
// Power-of-two array-length histogram (object vs primitive arrays). Always-on;
// mirrors render_md.rs::render_arrays_by_size.
function ArraysBySizeSection({ data, totalShallow }: { data?: ArraysBySize; totalShallow: number }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const obj = data?.obj_array_buckets ?? [];
  const prim = data?.prim_array_buckets ?? [];
  const zero = data?.zero_length_count ?? 0;
  const empty = obj.length === 0 && prim.length === 0 && zero === 0;

  const bucketTable = (title: string, buckets: ArraysBySize["obj_array_buckets"]) => {
    const totalObjects = buckets.reduce((s, b) => s + b.objects, 0);
    const totalBytes = buckets.reduce((s, b) => s + b.shallow, 0);
    type Bucket = (typeof buckets)[0];
    const cols: TableColumn<Bucket>[] = [
      { id: "len", name: "Max length", right: true, width: "120px", format: (b) => `≤ ${fmtCount(b.upper_len)}`, selector: (b) => b.upper_len },
      { id: "objects", name: "Objects", right: true, width: "110px", format: (b) => fmtCount(b.objects), selector: (b) => b.objects },
      { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: byteCell(b => b.shallow, fmtB, useKB), selector: (b) => b.shallow },
      { id: "pct", name: "% Heap", right: true, width: "90px", format: (b) => totalShallow > 0 ? fmtPct(b.shallow / totalShallow * 100) : "—", selector: (b) => b.shallow },
    ];
    return (
      <>
        <h3>{title}</h3>
        {buckets.length === 0 ? (
          <p className="subtitle">None.</p>
        ) : (
          <>
            <StdTable columns={cols} data={buckets} searchKeys={[]} fmtBtn={kbBtn} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ width: "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(totalObjects)}</span>
              <span style={{ width: useKB ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalBytes)}</span>
              <span style={{ width: "90px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{totalShallow > 0 ? fmtPct(totalBytes / totalShallow * 100) : "—"}</span>
              <span style={{ flex: 1 }} />
            </div>
          </>
        )}
      </>
    );
  };

  return (
    <section id="arrays-by-size">
      <h2>Arrays by Size</h2>
      <p className="subtitle">
        Array-length distribution bucketed by power-of-two element length; Max length is the inclusive upper bound of
        each bucket.
      </p>
      {empty ? (
        <p className="subtitle">No arrays found.</p>
      ) : (
        <>
          {bucketTable("Object arrays", obj)}
          {bucketTable("Primitive arrays", prim)}
          <p>Zero-length arrays: {fmtCount(zero)}</p>
        </>
      )}
    </section>
  );
}

// ── Collections ─────────────────────────────────────────────────────────────
// Collection/array occupancy: fill ratios, size distribution, map collision
// (load) ratio, and constant primitive arrays. Always-on; mirrors
// render_md.rs::render_collections.
function CollectionsSection({ data }: { data?: CollectionsAnalysis }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();          // Collections by Kind
  const [fmtBcfr, kbBtnCfr, useKBcfr] = useFmtBytes(); // Collection Fill Ratio
  const [fmtBcbs, kbBtnCbs, useKBcbs] = useFmtBytes(); // Collections by Size
  const [fmtBafr, kbBtnAfr, useKBafr] = useFmtBytes(); // Array Fill Ratio
  const [fmtBmcr, kbBtnMcr, useKBmcr] = useFmtBytes(); // Map Collision Ratio
  const [fmtBcpa, kbBtnCpa, useKBcpa] = useFmtBytes(); // Constant Primitive Arrays
  const [fmtBoarr, kbBtnOarr, useKBoarr] = useFmtBytes(); // Top Object Arrays
  const [fmtBparr, kbBtnParr, useKBparr] = useFmtBytes(); // Top Primitive Arrays
  const cfr = data?.collection_fill_ratio;
  const cbs = data?.collections_by_size;
  const afr = data?.array_fill_ratio;
  const mcr = data?.map_collision_ratio;
  const cpa = data?.constant_primitive_arrays;
  const topPrim = data?.top_prim_arrays;
  const topObj = data?.top_obj_arrays;

  // The two Top Arrays tables (largest individual arrays + largest array
  // classes by aggregate shallow) for one category. Mirrors
  // render_md.rs::render_top_arrays.
  const topArraysBlock = (t: TopArrays | undefined, kind: string, fmtBArr: (n: number) => string, kbBtnArr: React.ReactNode, useKBArr: boolean) => {
    const individual = t?.top_individual ?? [];
    const byClass = t?.top_by_class ?? [];
    const hasFill = individual.some((r) => r.non_null != null);
    const hasOwner = individual.some((r) => r.owner != null);
    const totalIndivShallow = individual.reduce((s, r) => s + r.shallow, 0);
    const indivCols: TableColumn<import("./types").TopArrayRow>[] = [
      { id: "class", name: "Array class", grow: 1, cell: (r) => <code>{r.array_class}</code> },
      { id: "length", name: "Length", right: true, width: "100px", format: (r) => fmtCount(r.length), selector: (r) => r.length },
      ...(hasFill ? [{ id: "fill", name: "Used/Length", right: true, width: "120px", selector: (r: import("./types").TopArrayRow) => r.non_null ?? 0, format: (r: import("./types").TopArrayRow) => r.non_null != null ? `${fmtCount(r.non_null)}/${fmtCount(r.length)}` : "—" } as TableColumn<import("./types").TopArrayRow>] : []),
      { id: "shallow", name: useKBArr ? "Shallow (KB)" : "Shallow", right: true, width: useKBArr ? "130px" : "110px", cell: byteCell(r => r.shallow, fmtBArr, useKBArr), selector: (r) => r.shallow },
      ...(hasOwner ? [{ id: "owner", name: "Owner (Class#field)", grow: 1, cell: (r: import("./types").TopArrayRow) => r.owner ? <code>{r.owner}</code> : <span>—</span> } as TableColumn<import("./types").TopArrayRow>] : []),
    ];
    const byClassCols: TableColumn<import("./types").TopArrayClassRow>[] = [
      { id: "class", name: "Array class", grow: 1, cell: (r) => <code>{r.array_class}</code> },
      { id: "instances", name: "Instances", right: true, width: "120px", format: (r) => fmtCount(r.objects), selector: (r) => r.objects },
      { id: "shallow", name: useKBArr ? "Shallow (KB)" : "Shallow", right: true, width: useKBArr ? "130px" : "110px", cell: byteCell(r => r.shallow, fmtBArr, useKBArr), selector: (r) => r.shallow },
    ];
    return (
      <>
        <h3>Top Arrays ({kind})</h3>
        <p className="subtitle">
          The largest {kind} arrays by shallow size, individually and aggregated by array class.
        </p>
        {individual.length === 0 ? (
          <p className="subtitle">None.</p>
        ) : (
          <>
            <StdTable columns={indivCols} data={individual} searchKeys={["array_class"]} fmtBtn={kbBtnArr} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}></span>
              {hasFill && <span style={{ width: "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}></span>}
              <span style={{ width: useKBArr ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtBArr(totalIndivShallow)}</span>
              {hasOwner && <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}></span>}
            </div>
          </>
        )}
        <h4>Top Array Classes ({kind})</h4>
        {byClass.length === 0 ? (
          <p className="subtitle">None.</p>
        ) : (
          <>
            <StdTable columns={byClassCols} data={byClass} searchKeys={["array_class"]} fmtBtn={kbBtnArr} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(byClass.reduce((s, r) => s + r.objects, 0))}</span>
              <span style={{ width: useKBArr ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtBArr(byClass.reduce((s, r) => s + r.shallow, 0))}</span>
            </div>
          </>
        )}
      </>
    );
  };

  // Format a basis-point fill/load range as a percent label (e.g. "0–10%").
  const ratioLabel = (b: FillRatioBucket) =>
    b.lower_ratio_bp === b.upper_ratio_bp
      ? `${b.lower_ratio_bp / 100}% (full)`
      : `${b.lower_ratio_bp / 100}–${b.upper_ratio_bp / 100}%`;

  // A fill/wasted table (Collection Fill Ratio, Array Fill Ratio) sharing 4 cols.
  const fillTable = (label: string, itemsHeader: string, buckets: FillRatioBucket[], fmtBFill: (n: number) => string, kbBtnFill: React.ReactNode, useKBFill: boolean) => {
    const totalItems = buckets.reduce((s, b) => s + b.objects, 0);
    const totalShallowFill = buckets.reduce((s, b) => s + b.shallow, 0);
    const totalWasted = buckets.reduce((s, b) => s + b.wasted, 0);
    const fillCols: TableColumn<FillRatioBucket>[] = [
      { id: "ratio", name: label, right: true, width: "130px", format: (b) => ratioLabel(b), selector: (b) => b.lower_ratio_bp },
      { id: "items", name: itemsHeader, right: true, width: "110px", format: (b) => fmtCount(b.objects), selector: (b) => b.objects },
      { id: "shallow", name: useKBFill ? "Shallow (KB)" : "Shallow", right: true, width: useKBFill ? "130px" : "110px", cell: byteCell(b => b.shallow, fmtBFill, useKBFill), selector: (b) => b.shallow },
      { id: "wasted", name: useKBFill ? "Wasted (KB)" : "Wasted", right: true, width: useKBFill ? "130px" : "110px", cell: byteCell(b => b.wasted, fmtBFill, useKBFill), selector: (b) => b.wasted },
    ];
    return (
      <>
        <StdTable columns={fillCols} data={buckets} searchKeys={[]} fmtBtn={kbBtnFill} />
        <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
          <span style={{ width: "130px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }}>Total</span>
          <span style={{ width: "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(totalItems)}</span>
          <span style={{ width: useKBFill ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtBFill(totalShallowFill)}</span>
          <span style={{ width: useKBFill ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtBFill(totalWasted)}</span>
          <span style={{ flex: 1 }} />
        </div>
      </>
    );
  };

  const cfrBuckets = cfr?.buckets ?? [];
  const cbsBuckets = cbs?.buckets ?? [];
  const afrBuckets = afr?.buckets ?? [];
  const mcrBuckets = mcr?.buckets ?? [];
  const cpaRows = cpa?.rows ?? [];
  const cpaHasOwner = cpaRows.some((r) => r.owner != null);
  const kindRows = data?.kind_summary?.kinds ?? [];

  return (
    <section id="collections">
      <h2>Collections</h2>
      <p className="subtitle">
        Collection and array occupancy: how full collections are, how big they get, and constant primitive arrays.
      </p>

      <h3>Collections by Kind</h3>
      {kindRows.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (() => {
        const kindCols: TableColumn<import("./types").CollectionKindStat>[] = [
          { id: "kind", name: "Kind", grow: 1, selector: (s) => s.kind },
          { id: "count", name: "Count", right: true, width: "100px", format: (s) => fmtCount(s.count), selector: (s) => s.count },
          { id: "total_el", name: "Total Elements", right: true, width: "130px", format: (s) => fmtCount(s.total_elements), selector: (s) => s.total_elements },
          { id: "max_el", name: "Max Elements", right: true, width: "130px", format: (s) => fmtCount(s.max_elements), selector: (s) => s.max_elements },
          { id: "shallow", name: useKB ? "Total Shallow (KB)" : "Total Shallow", right: true, width: useKB ? "150px" : "120px", cell: byteCell(s => s.total_shallow, fmtB, useKB), selector: (s) => s.total_shallow },
        ];
        return (
          <>
            <StdTable columns={kindCols} data={kindRows} searchKeys={["kind"]} fmtBtn={kbBtn} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(kindRows.reduce((s, r) => s + r.count, 0))}</span>
              <span style={{ width: "130px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(kindRows.reduce((s, r) => s + r.total_elements, 0))}</span>
              <span style={{ width: "130px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}></span>
              <span style={{ width: useKB ? "150px" : "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(kindRows.reduce((s, r) => s + r.total_shallow, 0))}</span>
            </div>
          </>
        );
      })()}

      <h3>Collection Fill Ratio</h3>
      <p className="subtitle">
        {fmtCount(cfr?.tracked ?? 0)} tracked of {fmtCount(cfr?.total ?? 0)} collections.
      </p>
      {cfrBuckets.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        fillTable("Fill %", "Collections", cfrBuckets, fmtBcfr, kbBtnCfr, useKBcfr)
      )}

      <h3>Collections by Size</h3>
      <p className="subtitle">
        {fmtCount(cbs?.tracked ?? 0)} tracked; {fmtCount(cbs?.empty_count ?? 0)} empty.
      </p>
      {cbsBuckets.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (() => {
        const cbsCols: TableColumn<import("./types").SizeHistogramBucket>[] = [
          { id: "size", name: "Size ≤", right: true, width: "120px", format: (b) => `≤ ${fmtCount(b.upper_len)}`, selector: (b) => b.upper_len },
          { id: "collections", name: "Collections", right: true, width: "120px", format: (b) => fmtCount(b.objects), selector: (b) => b.objects },
          { id: "shallow", name: useKBcbs ? "Shallow (KB)" : "Shallow", right: true, width: useKBcbs ? "140px" : "120px", cell: byteCell(b => b.shallow, fmtBcbs, useKBcbs), selector: (b) => b.shallow },
        ];
        return (
          <>
            <StdTable columns={cbsCols} data={cbsBuckets} searchKeys={[]} fmtBtn={kbBtnCbs} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ width: "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(cbsBuckets.reduce((s, b) => s + b.objects, 0))}</span>
              <span style={{ width: useKBcbs ? "140px" : "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtBcbs(cbsBuckets.reduce((s, b) => s + b.shallow, 0))}</span>
              <span style={{ flex: 1 }} />
            </div>
          </>
        );
      })()}

      <h3>Array Fill Ratio</h3>
      <p className="subtitle">{fmtCount(afr?.tracked ?? 0)} tracked object arrays.</p>
      {afrBuckets.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        fillTable("Fill %", "Arrays", afrBuckets, fmtBafr, kbBtnAfr, useKBafr)
      )}

      <h3>Map Collision Ratio</h3>
      <p className="subtitle">
        {fmtCount(mcr?.tracked ?? 0)} tracked of {fmtCount(mcr?.total ?? 0)} maps (occupied slots ÷ size; lower is
        worse).
      </p>
      {mcrBuckets.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (() => {
        const mcrCols: TableColumn<FillRatioBucket>[] = [
          { id: "load", name: "Load %", right: true, width: "130px", format: (b) => ratioLabel(b), selector: (b) => b.lower_ratio_bp },
          { id: "maps", name: "Maps", right: true, width: "110px", format: (b) => fmtCount(b.objects), selector: (b) => b.objects },
          { id: "shallow", name: useKBmcr ? "Shallow (KB)" : "Shallow", right: true, width: useKBmcr ? "130px" : "110px", cell: byteCell(b => b.shallow, fmtBmcr, useKBmcr), selector: (b) => b.shallow },
        ];
        return (
          <>
            <StdTable columns={mcrCols} data={mcrBuckets} searchKeys={[]} fmtBtn={kbBtnMcr} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ width: "130px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(mcrBuckets.reduce((s, b) => s + b.objects, 0))}</span>
              <span style={{ width: useKBmcr ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtBmcr(mcrBuckets.reduce((s, b) => s + b.shallow, 0))}</span>
              <span style={{ flex: 1 }} />
            </div>
          </>
        );
      })()}

      <h3>Constant Primitive Arrays</h3>
      <p className="subtitle">
        Primitive arrays whose every element is identical.
        {cpa?.truncated ? " (list truncated; remaining groups folded into one row)." : ""}
      </p>
      {cpaRows.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (() => {
        const cpaCols: TableColumn<import("./types").ConstantArrayRow>[] = [
          { id: "class", name: "Array class", grow: 1, cell: (r) => <code>{r.array_class}</code> },
          { id: "length", name: "Length", right: true, width: "100px", format: (r) => fmtCount(r.length), selector: (r) => r.length },
          { id: "value", name: "Value", right: true, width: "90px", format: (r) => String(r.value), selector: (r) => r.value },
          { id: "objects", name: "Objects", right: true, width: "100px", format: (r) => fmtCount(r.objects), selector: (r) => r.objects },
          { id: "shallow", name: useKBcpa ? "Shallow (KB)" : "Shallow", right: true, width: useKBcpa ? "130px" : "110px", cell: byteCell(r => r.shallow, fmtBcpa, useKBcpa), selector: (r) => r.shallow },
          ...(cpaHasOwner ? [{ id: "owner", name: "Owner (Class#field)", grow: 1, cell: (r: import("./types").ConstantArrayRow) => r.owner ? <code>{r.owner}</code> : <span>—</span> } as TableColumn<import("./types").ConstantArrayRow>] : []),
        ];
        return <StdTable columns={cpaCols} data={cpaRows} searchKeys={["array_class"]} fmtBtn={kbBtnCpa} />;
      })()}

      {topArraysBlock(topPrim, "primitive", fmtBparr, kbBtnParr, useKBparr)}
      {topArraysBlock(topObj, "object", fmtBoarr, kbBtnOarr, useKBoarr)}
    </section>
  );
}

// ── Container Attribution (Class#field) ──────────────────────────────────────
// Which holder Class#field points at the most container memory. Absent when
// --collections was off (data undefined → section not rendered). Mirrors
// render_md.rs::render_collection_attribution (HTML has no bar columns).
function TinyCollectionTable({ rows }: { rows: import("./types").TinyCollectionRow[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const cols: TableColumn<import("./types").TinyCollectionRow>[] = [
    { id: "field", name: "Class#field", grow: 1, cell: (r) => <code>{r.holder_class}#{r.field}</code>, selector: (r) => `${r.holder_class}#${r.field}` },
    { id: "kind", name: "Kind", width: "100px", selector: (r) => r.container_kind },
    { id: "empty", name: "Empty", right: true, width: "90px", format: (r) => fmtCount(r.empty_count), selector: (r) => r.empty_count },
    { id: "singleton", name: "Singleton", right: true, width: "100px", format: (r) => fmtCount(r.singleton_count), selector: (r) => r.singleton_count },
    { id: "overhead", name: useKB ? "Overhead (KB)" : "Overhead Bytes", right: true, width: useKB ? "150px" : "130px", cell: byteCell(r => r.overhead_bytes, fmtB, useKB), selector: (r) => r.overhead_bytes },
  ];
  return <StdTable columns={cols} data={rows} searchKeys={["holder_class"]} fmtBtn={kbBtn} />;
}

function CollectionAttributionSection({ data }: { data?: CollectionAttribution }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  if (!data) return null;
  const mostOverall = data.most_overall ?? [];
  const biggestSingle = data.biggest_single ?? [];
  return (
    <section id="container-attribution-classfield">
      <h2>Container Attribution (Class#field)</h2>
      <p className="subtitle">
        Which holder <code>Class#field</code> points at the most container memory. Two rankings: total across
        all containers reached through a field, and the single largest container per field.
      </p>

      <h3>Most Overall</h3>
      {mostOverall.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (() => {
        const overallCols: TableColumn<import("./types").FieldAttributionRow>[] = [
          { id: "field", name: "Class#field", grow: 1, cell: (r) => <code>{r.holder_class}#{r.field}</code> },
          { id: "kind", name: "Kind", width: "100px", selector: (r) => r.container_kind },
          { id: "containers", name: "Containers", right: true, width: "120px", format: (r) => fmtCount(r.container_count), selector: (r) => r.container_count },
          { id: "holders", name: "Holder Instances", right: true, width: "140px", format: (r) => fmtCount(r.holder_instances), selector: (r) => r.holder_instances },
          { id: "elements", name: "Total Elements", right: true, width: "130px", format: (r) => fmtCount(r.total_elements), selector: (r) => r.total_elements },
          { id: "retained", name: useKB ? "Total Retained (KB)" : "Total Retained", right: true, width: useKB ? "160px" : "130px", cell: byteCell(r => r.total_retained, fmtB, useKB), selector: (r) => r.total_retained },
          { id: "wasted", name: useKB ? "Wasted (KB)" : "Wasted Bytes", right: true, width: useKB ? "140px" : "120px", cell: (r) => r.total_wasted_bytes != null ? (useKB ? <span title={fmtExactBytes(r.total_wasted_bytes)}>{fmtB(r.total_wasted_bytes)}</span> : fmtB(r.total_wasted_bytes)) : "—", selector: (r) => r.total_wasted_bytes ?? 0 },
        ];
        return <StdTable columns={overallCols} data={mostOverall} searchKeys={["holder_class"]} fmtBtn={kbBtn} />;
      })()}

      <h3>Biggest Single</h3>
      {biggestSingle.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (() => {
        const singleCols: TableColumn<import("./types").FieldAttributionBiggestRow>[] = [
          { id: "field", name: "Class#field", grow: 1, cell: (r) => <code>{r.holder_class}#{r.field}</code> },
          { id: "container", name: "Container Class", grow: 1, cell: (r) => <code>{r.container_class}</code> },
          { id: "kind", name: "Kind", width: "100px", selector: (r) => r.container_kind },
          { id: "elements", name: "Elements", right: true, width: "100px", format: (r) => fmtCount(r.elements), selector: (r) => r.elements },
          { id: "capacity", name: "Capacity", right: true, width: "100px", format: (r) => fmtCount(r.capacity), selector: (r) => r.capacity },
          { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.retained, fmtB, useKB), selector: (r) => r.retained },
        ];
        return <StdTable columns={singleCols} data={biggestSingle} searchKeys={["holder_class"]} fmtBtn={kbBtn} />;
      })()}

      {data.tiny_overhead && data.tiny_overhead.length > 0 && (
        <>
          <h3>Tiny Collection Overhead</h3>
          <p className="subtitle">
            Empty (size-0) and singleton (size-1) collections whose wrapper objects are pure overhead.
            Overhead bytes = object count × reference-slot width.
          </p>
          <TinyCollectionTable rows={data.tiny_overhead} />
        </>
      )}

      {data.truncated && (
        <p className="subtitle">
          Attribution data was truncated (holder-edge or container-record cap hit); rankings are a
          bounded sample.
        </p>
      )}
    </section>
  );
}

function BiggestCollectionsTable({ rows, title }: { rows: BiggestCollectionRow[]; title: string }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  if (rows.length === 0) return null;
  const hasRetained = rows.some((r) => r.retained != null);
  const hasOwner = rows.some((r) => r.owner != null);
  const hasBreakdown = rows.some((r) => (r.value_type_breakdown?.length ?? 0) > 0);
  // Drop standalone Value Type column when breakdown is present (it duplicates the lead entry).
  const hasValue = !hasBreakdown && rows.some((r) => r.dominant_value_type != null);
  const totalElements = rows.reduce((s, r) => s + r.elements, 0);
  const totalRetained = rows.reduce((s, r) => s + (r.retained ?? 0), 0);

  // Coalesce consecutive identical rows.
  type Coalesced = { row: BiggestCollectionRow; count: number };
  const coalesced: Coalesced[] = [];
  for (const r of rows) {
    const last = coalesced[coalesced.length - 1];
    if (
      last &&
      last.row.kind === r.kind &&
      last.row.container_class === r.container_class &&
      last.row.elements === r.elements &&
      last.row.owner === r.owner &&
      last.row.retained === r.retained
    ) {
      last.count++;
    } else {
      coalesced.push({ row: r, count: 1 });
    }
  }

  type CoalescedRow = { row: BiggestCollectionRow; count: number };
  const cols: TableColumn<CoalescedRow>[] = [
    { id: "kind", name: "Kind", width: "80px", selector: ({ row: r }) => r.kind },
    {
      id: "class", name: "Container Class", grow: 1,
      cell: ({ row: r, count }) => (
        <>
          <code>{r.container_class}</code>
          {count > 1 && <span className="muted"> ×{fmtCount(count)}</span>}
        </>
      ),
    },
    { id: "elements", name: "Elements", right: true, width: "100px", format: ({ row: r, count }) => count > 1 ? `${fmtCount(r.elements)} each` : fmtCount(r.elements), selector: ({ row: r }) => r.elements },
    ...(hasValue ? [{ id: "value", name: "Value Type", grow: 1, cell: ({ row: r }: CoalescedRow) => r.dominant_value_type ? <code>{r.dominant_value_type}</code> : <span>—</span> } as TableColumn<CoalescedRow>] : []),
    ...(hasBreakdown ? [{
      id: "breakdown", name: "Value Types (top)", grow: 2,
      cell: ({ row: r }: CoalescedRow) => !r.value_type_breakdown || r.value_type_breakdown.length === 0
        ? <span>—</span>
        : <>{r.value_type_breakdown.map((s, j) => <span key={j}>{j > 0 ? ", " : ""}<code>{s.type_name}</code> ×{fmtCount(s.count)}</span>)}</>,
    } as TableColumn<CoalescedRow>] : []),
    ...(hasOwner ? [{ id: "owner", name: "Owner (Class#field)", grow: 1, cell: ({ row: r }: CoalescedRow) => r.owner ? <code>{r.owner}</code> : <span>—</span> } as TableColumn<CoalescedRow>] : []),
    ...(hasRetained ? [{ id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: ({ row: r }: CoalescedRow) => r.retained != null ? (useKB ? <span title={fmtExactBytes(r.retained)}>{fmtB(r.retained)}</span> : fmtB(r.retained)) : "—", selector: ({ row: r }: CoalescedRow) => r.retained ?? 0 } as TableColumn<CoalescedRow>] : []),
  ];
  return (
    <>
      <h3>{title}</h3>
      <StdTable columns={cols} data={coalesced} searchKeys={[]} fmtBtn={kbBtn} />
      <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
        <span style={{ width: "80px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }}></span>
        <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
        <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(totalElements)}</span>
        {hasValue && <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}></span>}
        {hasBreakdown && <span style={{ flex: 2, paddingLeft: 5, paddingRight: 5 }}></span>}
        {hasOwner && <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}></span>}
        {hasRetained && <span style={{ width: useKB ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalRetained)}</span>}
      </div>
    </>
  );
}

function BiggestCollectionsSection({ data }: { data?: BiggestCollections }) {
  if (!data) return null;
  return (
    <section id="biggest-collections">
      <h2>Biggest Collections</h2>
      <p className="subtitle">
        The largest individual collection instances. Owner is the primary incoming <code>Class#field</code>;
        value type is the dominant runtime element type (<code>varies</code> when none dominates).
        Owner/retained/value columns require <code>--collections</code>.
      </p>
      <BiggestCollectionsTable rows={data.combined} title="Combined" />
      {data.by_kind.map((k) => <BiggestCollectionsTable key={k.kind} rows={k.rows} title={`By Kind — ${k.kind}`} />)}
      {data.truncated && (
        <p className="subtitle">Collection value tally was truncated; ranking is a bounded sample.</p>
      )}
    </section>
  );
}

function CollectionContentsSection({ data }: { data?: CollectionContents }) {
  if (!data) return null;
  const rows = data.rows ?? [];
  const cols: TableColumn<import("./types").CollectionContentsRow>[] = [
    { id: "class", name: "Collection Class", grow: 1, cell: (r) => <code>{r.collection_class}</code> },
    { id: "instances", name: "Instances", right: true, width: "120px", format: (r) => fmtCount(r.instances), selector: (r) => r.instances },
    { id: "values", name: "Total Values", right: true, width: "120px", format: (r) => fmtCount(r.total_values), selector: (r) => r.total_values },
    {
      id: "types", name: "Top Value Types", grow: 2,
      cell: (r) => r.top_value_types.length === 0
        ? <span>—</span>
        : <>{r.top_value_types.map((s, j) => <span key={j}>{j > 0 ? ", " : ""}<code>{s.type_name}</code> ×{fmtCount(s.count)}</span>)}</>,
    },
  ];
  return (
    <section id="collection-contents-by-type">
      <h2>Collection Contents by Type</h2>
      <p className="subtitle">
        What runtime element/value types your collections hold, aggregated per collection class.
        Requires <code>--collections</code>.
      </p>
      {rows.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        <StdTable columns={cols} data={rows} searchKeys={["collection_class"]} />
      )}
      {data.truncated && (
        <p className="subtitle">Truncated; a bounded sample of collection classes is shown.</p>
      )}
    </section>
  );
}

// ── Fields by Retained Size (Class#field) ────────────────────────────────────
// Which holder Class#field retains the most memory summed over its pointees.
// Absent when --collections was off. Mirrors render_md.rs::render_fields_by_size
// (HTML has no bar column).
function FieldsBySizeSection({ data }: { data?: FieldsBySize }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  if (!data) return null;
  const rows = data.rows ?? [];
  const totalRetained = rows.reduce((s, r) => s + r.total_retained, 0);
  const totalPointees = rows.reduce((s, r) => s + r.pointees, 0);
  const hasElements = rows.some((r) => (r.elements ?? 0) > 0);
  type FBSRow = import("./types").FieldBySizeRow;
  const cols: TableColumn<FBSRow>[] = [
    { id: "field", name: "Class#field", grow: 1, cell: (r) => <code>{r.holder_class}#{r.field}</code> },
    { id: "pointee", name: "Runtime Pointee Type", grow: 1, cell: (r) => <code>{r.pointee_type}</code> },
    { id: "category", name: "Category", width: "100px", selector: (r) => r.category ?? "", format: (r) => r.category ?? "—" },
    { id: "pointees", name: "Pointees", right: true, width: "100px", format: (r) => fmtCount(r.pointees), selector: (r) => r.pointees },
    ...(hasElements ? [{ id: "elements", name: "Elements", right: true, width: "100px", format: (r: FBSRow) => r.elements != null ? fmtCount(r.elements) : "—", selector: (r: FBSRow) => r.elements ?? 0 } as TableColumn<FBSRow>] : []),
    { id: "holders", name: "Holder Instances", right: true, width: "140px", format: (r) => fmtCount(r.holder_instances), selector: (r) => r.holder_instances },
    { id: "sharing", name: "Sharing", right: true, width: "90px", selector: (r) => r.holder_instances > 0 ? r.pointees / r.holder_instances : 0, format: (r) => r.holder_instances > 0 ? `${(r.pointees / r.holder_instances).toFixed(1)}×` : "—" },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.total_retained, fmtB, useKB), selector: (r) => r.total_retained },
  ];
  return (
    <section id="fields-by-retained-size-classfield">
      <h2>Fields by Retained Size (Class#field)</h2>
      {data.truncated && (
        <p className="subtitle">
          Field grouping was truncated (group or pointee cap hit); ranking is a bounded sample.
        </p>
      )}
      <p className="subtitle">
        Which holder <code>Class#field</code> retains the most memory, summed over every object the
        field points at. Runtime pointee type is the dominant concrete class reached through the
        field (<code>varies</code> when no single type dominates).
      </p>
      {rows.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        <>
          <StdTable columns={cols} data={rows} searchKeys={["holder_class"]} fmtBtn={kbBtn} />
          <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
            <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
            <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}></span>
            <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }}></span>
            <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(totalPointees)}</span>
            {hasElements && <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(rows.reduce((s, r) => s + (r.elements ?? 0), 0))}</span>}
            <span style={{ width: "140px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }}></span>
            <span style={{ width: "90px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5 }}></span>
            <span style={{ width: useKB ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalRetained)}</span>
          </div>
        </>
      )}
    </section>
  );
}

// ── References ──────────────────────────────────────────────────────────────
// Soft/weak/phantom reference referents (what they point at). Always-on;
// mirrors render_md.rs::render_references.
function RefClassTable({ rows }: { rows: RefStatClassRow[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const cols: TableColumn<RefStatClassRow>[] = [
    { id: "class", name: "Class", grow: 1, cell: (r) => <code>{r.pretty_class}</code> },
    { id: "objects", name: "Objects", right: true, width: "100px", format: (r) => fmtCount(r.objects), selector: (r) => r.objects },
    { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.shallow, fmtB, useKB), selector: (r) => r.shallow },
    { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.retained ?? 0, fmtB, useKB), selector: (r) => r.retained ?? 0 },
  ];
  return (
    <>
      <StdTable columns={cols} data={rows} searchKeys={["pretty_class"]} fmtBtn={kbBtn} />
      <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
        <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
        <span style={{ width: "100px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(rows.reduce((s, r) => s + r.objects, 0))}</span>
        <span style={{ width: useKB ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(rows.reduce((s, r) => s + r.shallow, 0))}</span>
        <span style={{ width: useKB ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(rows.reduce((s, r) => s + (r.retained ?? 0), 0))}</span>
      </div>
    </>
  );
}

function ReferencesSection({ data }: { data?: ReferencesAnalysis }) {
  const kinds: ReferenceStats[] = [data?.soft, data?.weak, data?.phantom].filter(
    (s): s is ReferenceStats => s != null,
  );

  const kindCaption = (kind: string) => {
    switch (kind) {
      case "Soft": return "Soft references keep objects alive until the JVM needs memory — cleared under GC pressure. A large soft-referenced heap is often an unbounded cache; consider bounding the cache size.";
      case "Weak": return "Weak references do not prevent GC. Objects listed here are reachable only via weak chains — under any GC they may be reclaimed. Large counts are usually benign.";
      case "Phantom": return "Phantom references mark objects in finalization or cleanup pipelines. A large backlog may indicate that the ReferenceQueue processor is too slow or blocked, or that native resources are not being released promptly.";
      default: return "";
    }
  };

  return (
    <section id="references">
      <h2>References</h2>
      <p className="subtitle">Soft/weak/phantom reference referents (what they point at).</p>
      {kinds.length === 0 ? (
        <p className="subtitle">No soft, weak, or phantom references found.</p>
      ) : (
        kinds.map((stats) => (
          <React.Fragment key={stats.kind}>
            <h3>{stats.kind} References</h3>
            <p className="subtitle">{kindCaption(stats.kind)}</p>
            <p className="subtitle">{fmtCount(stats.reference_instances)} reference instances.</p>
            <h4>Referent classes</h4>
            <RefClassTable rows={stats.referent_histogram ?? []} />
            <h4>Only-weakly retained (approximate)</h4>
            <p className="subtitle">Objects with no incoming strong reference other than this reference chain — GC pressure would free them.</p>
            {(stats.only_weakly_retained ?? []).length > 0
              ? <RefClassTable rows={stats.only_weakly_retained} />
              : <p className="subtitle"><em>None found — no objects are exclusively reachable via this reference kind.</em></p>
            }
          </React.Fragment>
        ))
      )}
    </section>
  );
}

// ── Dominator Analysis ──────────────────────────────────────────────────────
// Two dominator-tree sub-views: Big Drops (dominators where retained heap
// concentrates) and Immediate Dominators (dominated-object rollup by dominator
// class). Always-on; mirrors render_md.rs::render_dominator_analysis.
function DominatorAnalysisSection({ data }: { data?: DominatorAnalysis }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const drops = data?.big_drops?.rows ?? [];
  const threshold = data?.big_drops?.threshold ?? 0;
  const thresholdMb = (threshold / (1024 * 1024)).toFixed(1);
  const idoms = data?.immediate_dominators?.rows ?? [];
  return (
    <section id="dominator-analysis">
      <h2>Dominator Analysis</h2>

      <h3>Big Drops</h3>
      <p className="subtitle">
        Dominators where retained heap concentrates: retained heap minus the largest single child. Threshold{" "}
        {thresholdMb} MB (1% of reachable shallow).
      </p>
      {drops.length === 0 ? (
        <p className="subtitle">No significant drops.</p>
      ) : (() => {
        const dropCols: TableColumn<import("./types").BigDropRow>[] = [
          { id: "object", name: "Object", grow: 1, cell: (r) => <span className="copy-cell"><code>{r.display_class}</code><CopyBtn text={r.display_class} /></span> },
          { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.retained, fmtB, useKB), selector: (r) => r.retained },
          { id: "largest_child", name: "Largest Child", grow: 1, cell: (r) => r.largest_child_class ? <code>{r.largest_child_class}</code> : <span>—</span> },
          { id: "child_ret", name: useKB ? "Child Ret. (KB)" : "Child Ret.", right: true, width: useKB ? "140px" : "110px", cell: byteCell(r => r.largest_child_retained, fmtB, useKB), selector: (r) => r.largest_child_retained },
          { id: "drop", name: useKB ? "Drop (KB)" : "Drop", right: true, width: useKB ? "120px" : "110px", cell: byteCell(r => r.drop_bytes, fmtB, useKB), selector: (r) => r.drop_bytes },
        ];
        const totalDropRetained = drops.reduce((s, r) => s + r.retained, 0);
        const totalChildRetained = drops.reduce((s, r) => s + r.largest_child_retained, 0);
        const totalDropBytes = drops.reduce((s, r) => s + r.drop_bytes, 0);
        return (
          <>
            <StdTable columns={dropCols} data={drops} searchKeys={["display_class"]} fmtBtn={kbBtn} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: useKB ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalDropRetained)}</span>
              <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}></span>
              <span style={{ width: useKB ? "140px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalChildRetained)}</span>
              <span style={{ width: useKB ? "120px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalDropBytes)}</span>
            </div>
          </>
        );
      })()}

      <h3>Immediate Dominators</h3>
      <p className="subtitle">
        Objects immediately dominated, rolled up by the dominator's class; a heavy dominated shallow heap under one
        class flags a retention hub.
      </p>
      {idoms.length === 0 ? (
        <p className="subtitle">No immediate dominators.</p>
      ) : (() => {
        const idomCols: TableColumn<import("./types").ImmediateDominatorRow>[] = [
          { id: "dominator_class", name: "Dominator Class", grow: 1, cell: (r) => <span className="copy-cell"><code>{r.dominator_class}</code><CopyBtn text={r.dominator_class} /></span> },
          { id: "dominator_count", name: "#Dominators", right: true, width: "132px", format: (r) => fmtCount(r.dominator_count), selector: (r) => r.dominator_count },
          { id: "dominated_count", name: "#Dominated", right: true, width: "120px", format: (r) => fmtCount(r.dominated_count), selector: (r) => r.dominated_count },
          { id: "dominator_shallow", name: useKB ? "Dom. Shallow (KB)" : "Dom. Shallow", right: true, width: useKB ? "150px" : "120px", cell: byteCell(r => r.dominator_shallow, fmtB, useKB), selector: (r) => r.dominator_shallow },
          { id: "dominated_shallow", name: useKB ? "Dominated Shallow (KB)" : "Dominated Shallow", right: true, width: useKB ? "175px" : "155px", cell: byteCell(r => r.dominated_shallow, fmtB, useKB), selector: (r) => r.dominated_shallow },
        ];
        const totalDomCount = idoms.reduce((s, r) => s + r.dominator_count, 0);
        const totalDominatedCount = idoms.reduce((s, r) => s + r.dominated_count, 0);
        const totalDomShallow = idoms.reduce((s, r) => s + r.dominator_shallow, 0);
        const totalDominatedShallow = idoms.reduce((s, r) => s + r.dominated_shallow, 0);
        return (
          <>
            <StdTable columns={idomCols} data={idoms} searchKeys={["dominator_class"]} fmtBtn={kbBtn} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: "132px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(totalDomCount)}</span>
              <span style={{ width: "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(totalDominatedCount)}</span>
              <span style={{ width: useKB ? "150px" : "120px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalDomShallow)}</span>
              <span style={{ width: useKB ? "175px" : "155px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalDominatedShallow)}</span>
            </div>
          </>
        );
      })()}
    </section>
  );
}

// ── Unreachable Objects ─────────────────────────────────────────────────────
// Per-class histogram of objects not dominated by the virtual root
// (idom == u32::MAX). Always-on; mirrors render_md.rs::render_unreachable_histogram.
type UnreachableKey = "objects" | "shallow" | "retained";
const UNREACHABLE_COLS: { key: UnreachableKey; label: string }[] = [
  { key: "objects", label: "Objects" },
  { key: "shallow", label: "Shallow" },
  { key: "retained", label: "Retained" },
];

function UnreachableCompositionTable({ comp }: { comp: HeapComposition }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  if (comp.by_kind.length === 0) return null;
  // When prim_array_by_type is available, expand "Primitive arrays" into
  // individual types so the chart shows byte[], int[], char[], etc.
  const chartKinds: KindStat[] = React.useMemo(() => {
    if (!comp.prim_array_by_type?.length) return comp.by_kind;
    return comp.by_kind.flatMap((k) =>
      k.kind === "Primitive arrays" ? comp.prim_array_by_type! : [k]
    );
  }, [comp]);
  return (
    <>
      <h3>Unreachable Heap Composition</h3>
      <ChartOrNote hasData={chartKinds.length >= 2} note="Composition chart needs at least two kinds; showing the table only.">
        <CompositionStackedBar data={chartKinds} />
      </ChartOrNote>
      {(() => {
        type CompRow = { kind: string; objects: number; shallow_heap: number; indent?: boolean };
        const flatRows: CompRow[] = comp.by_kind.flatMap((k) => {
          const main: CompRow = { kind: k.kind, objects: k.objects, shallow_heap: k.shallow_heap };
          if (k.kind === "Primitive arrays" && comp.prim_array_by_type?.length) {
            return [main, ...comp.prim_array_by_type.map((p) => ({ kind: p.kind, objects: p.objects, shallow_heap: p.shallow_heap, indent: true }))];
          }
          return [main];
        });
        const compCols: TableColumn<CompRow>[] = [
          { id: "kind", name: "Kind", grow: 1, cell: (r) => <span style={r.indent ? { paddingLeft: "1.5rem", fontSize: "0.88em", color: "var(--muted)" } : undefined}>{r.kind}</span> },
          { id: "objects", name: "Objects", right: true, width: "110px", cell: (r) => <span style={r.indent ? { fontSize: "0.88em", color: "var(--muted)" } : undefined}>{fmtCount(r.objects)}</span> },
          { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: (r) => <span title={useKB ? fmtExactBytes(r.shallow_heap) : undefined} style={r.indent ? { fontSize: "0.88em", color: "var(--muted)" } : undefined}>{fmtB(r.shallow_heap)}</span> },
        ];
        return <StdTable columns={compCols} data={flatRows} searchKeys={["kind"]} fmtBtn={kbBtn} />;
      })()}
    </>
  );
}

function UnreachableObjectsSection({ data }: { data?: SystemOverview }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const rows: UnreachableClassRow[] = data?.unreachable_histogram ?? [];
  const unreachablePct = React.useMemo(() => {
    const total = (data?.total_shallow ?? 0) + (data?.unreachable_shallow ?? 0);
    return total > 0 ? (data?.unreachable_shallow ?? 0) / total * 100 : 0;
  }, [data]);
  return (
    <section id="unreachable-objects">
      <h2>Unreachable Objects</h2>
      {rows.length === 0 ? (
        <p className="subtitle">No unreachable objects.</p>
      ) : (
        <>
          <p className="subtitle">
            {fmtCount(data?.unreachable_count ?? 0)} unreachable objects,{" "}
            {fmtB(data?.unreachable_shallow ?? 0)} shallow heap
            {` (within the unreachable forest retained = shallow since all paths stay in-forest; top ${fmtCount(rows.length)} classes by shallow).`}
          </p>
          <p className="subtitle">
            {unreachablePct >= 5
              ? `Unreachable objects are eligible for collection but have not yet been reclaimed. At ${fmtPct(unreachablePct)} of heap total (reachable + unreachable) this is elevated — the JVM may not have had time to GC before the dump was taken, or finalization may be backed up.`
              : "Unreachable objects are eligible for collection but have not yet been reclaimed. A small unreachable heap (< 5% of heap total) is normal between GC cycles."}
          </p>
          {data?.unreachable_composition && (
            <UnreachableCompositionTable comp={data.unreachable_composition} />
          )}
          {data?.unreachable_garbage_roots && data.unreachable_garbage_roots.length > 0 && (
            <UnreachableDomTreeSection roots={data.unreachable_garbage_roots} />
          )}
          <details open>
            <summary>Unreachable objects by class ({fmtCount(rows.length)} rows)</summary>
            {(() => {
              const unreachCols: TableColumn<UnreachableClassRow>[] = [
                { id: "class", name: "Class", grow: 1, cell: (r) => <code>{r.pretty_class}</code> },
                { id: "objects", name: "Objects", right: true, width: "110px", format: (r) => fmtCount(r.objects), selector: (r) => r.objects, sortable: true },
                { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.shallow, fmtB, useKB), selector: (r) => r.shallow, sortable: true },
                { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "130px" : "110px", cell: byteCell(r => r.retained, fmtB, useKB), selector: (r) => r.retained, sortable: true },
              ];
              return (
                <>
                  <StdTable columns={unreachCols} data={rows} searchKeys={["pretty_class"]} fmtBtn={kbBtn} defaultSortFieldId="shallow" />
                  <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
                    <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
                    <span style={{ width: "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(data?.unreachable_count ?? 0)}</span>
                    <span style={{ width: useKB ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(data?.unreachable_shallow ?? 0)}</span>
                    <span style={{ width: useKB ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(data?.unreachable_retained ?? 0)}</span>
                  </div>
                </>
              );
            })()}
          </details>
        </>
      )}
    </section>
  );
}

// ── Allocation Sites ──────────────────────────────────────────────────────────
// aggregated allocation sites. Honest note when the
// dump carried no allocation stack-trace info. Mirrors report.rs::render_alloc_sites.
function AllocSitesSection({ data }: { data: AllocSites }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  return (
    <section id="allocation-sites">
      <h2>Allocation Sites</h2>
      <p className="subtitle">Objects grouped by the stack trace that allocated them. Shallow heap is additive; retained is omitted because summing per-object retained over-counts shared subgraphs.</p>
      {!data.traces_present ? (
        <p className="subtitle">
          Allocation tracking was off in this dump (<code>stack_trace_serial = 0</code>); no allocation sites available.
        </p>
      ) : (() => {
        const allocCols: TableColumn<import("./types").AllocSite>[] = [
          { id: "stack", name: "Stack", grow: 1, cell: (s) => s.frames.length === 0 ? (
            <span className="hint">serial {s.stack_serial} <span className="hint">(no frames recorded)</span></span>
          ) : s.frames.length === 1 ? (
            <code>{s.frames[0]}</code>
          ) : (
            <details className="stack-detail">
              <summary><code>{s.frames[0]}</code></summary>
              <ol className="stack-frames">
                {s.frames.map((f, fi) => (
                  <li key={fi}><code>{f}</code></li>
                ))}
              </ol>
            </details>
          )},
          { id: "objects", name: "Objects", right: true, width: "110px", format: (s) => fmtCount(s.object_count), selector: (s) => s.object_count },
          { id: "shallow", name: useKB ? "Shallow (KB)" : "Shallow", right: true, width: useKB ? "130px" : "110px", cell: byteCell(s => s.shallow_total, fmtB, useKB), selector: (s) => s.shallow_total },
        ];
        const totalObjects = data.sites.reduce((s, r) => s + r.object_count, 0);
        const totalShallow = data.sites.reduce((s, r) => s + r.shallow_total, 0);
        return (
          <>
            <StdTable columns={allocCols} data={data.sites} searchKeys={[]} fmtBtn={kbBtn} />
            <div style={{ display: "flex", fontSize: "0.86rem", fontWeight: 600, borderTop: "2px solid var(--border)", paddingTop: "0.3rem", marginBottom: "1rem", fontVariantNumeric: "tabular-nums" }}>
              <span style={{ flex: 1, paddingLeft: 5, paddingRight: 5 }}>Total</span>
              <span style={{ width: "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtCount(totalObjects)}</span>
              <span style={{ width: useKB ? "130px" : "110px", flexShrink: 0, flexGrow: 0, paddingLeft: 5, paddingRight: 5, textAlign: "right" }}>{fmtB(totalShallow)}</span>
            </div>
          </>
        );
      })()}
    </section>
  );
}

// ── Retention Concentration ─────────────────────────────────────────────────
function RetentionConcentrationSection({ report }: { report: Report }) {
  const rc = report.overview.retention_concentration;
  if (!rc || (rc.top1_bp === 0 && rc.top10_bp === 0 && rc.top100_bp === 0 && rc.num_objects_ge_1pct === 0)) {
    return null;
  }
  return (
    <section id="retention-concentration">
      <h2>Retention Concentration</h2>
      <p className="subtitle">
        Share of the reachable heap retained by the few largest top-level dominators. If{" "}
        <strong>Top 1</strong> is already high, freeing that one object reclaims most memory; if
        the share only climbs as you widen to <strong>Top 10</strong> / <strong>Top 100</strong>,
        the leak is spread across many peers.
      </p>
      <ConcentrationChart rc={rc} />
      <ConcentrationStackedBar rc={rc} />
      {(() => {
        type RcRow = { scope: string; bp: number };
        const rcRows: RcRow[] = [
          { scope: "Top 1 object", bp: rc.top1_bp },
          { scope: "Top 10 objects", bp: rc.top10_bp },
          { scope: "Top 100 objects", bp: rc.top100_bp },
        ];
        const rcCols: TableColumn<RcRow>[] = [
          { id: "scope", name: "Scope", grow: 1, selector: (r) => r.scope },
          { id: "share", name: "Retained Share", right: true, width: "140px", selector: (r) => r.bp, format: (r) => fmtPct(r.bp / 100) },
        ];
        return <StdTable columns={rcCols} data={rcRows} searchKeys={[]} />;
      })()}
      {rc.num_objects_ge_1pct > 0 && (
        <p className="subtitle"><em>{fmtCount(rc.num_objects_ge_1pct)} {rc.num_objects_ge_1pct === 1 ? "object" : "objects"} each hold ≥1% of the reachable heap.</em></p>
      )}
    </section>
  );
}

// ── Dominator-Depth Distribution ─────────────────────────────────────────────
// Objects per idom-hop below a GC root. Mirrors render_md.rs::render_dominator_depth.
function DominatorDepthSection({ report }: { report: Report }) {
  const hist = report.overview.dominator_depth_histogram;

  const totalObjs = (hist ?? []).reduce((s, b) => s + b.objects, 0);
  const maxDepth = (hist ?? []).reduce((m, b) => Math.max(m, b.depth), 0);

  // Compute cumulative percentage for each bucket.
  type DepthRow = { depth: number; objects: number; pct: number; cum: number };
  const rows: DepthRow[] = React.useMemo(() => {
    if (!hist) return [];
    let cumSum = 0;
    return hist.map((b) => {
      cumSum += b.objects;
      return {
        depth: b.depth,
        objects: b.objects,
        pct: totalObjs > 0 ? (b.objects / totalObjs) * 100 : 0,
        cum: totalObjs > 0 ? (cumSum / totalObjs) * 100 : 0,
      };
    });
  }, [hist, totalObjs]);

  if (!hist || hist.length === 0) return null;

  const depthCols: TableColumn<DepthRow>[] = [
    { id: "depth", name: "Depth", right: true, width: "80px", selector: (r) => r.depth },
    { id: "objects", name: "Objects", right: true, width: "110px", format: (r) => fmtCount(r.objects), selector: (r) => r.objects },
    { id: "pct", name: "% Objects", right: true, width: "110px", format: (r) => fmtPct(r.pct), selector: (r) => r.pct },
    { id: "cum", name: "Cumulative %", right: true, width: "120px", format: (r) => fmtPct(r.cum), selector: (r) => r.cum },
  ];

  return (
    <section id="dominator-depth-distribution">
      <h2>Dominator-Depth Distribution</h2>
      <p className="subtitle">
        Objects per idom-hop below a GC root. Shallow depth means most objects are held close to a
        root; deep depth means retention flows through long chains (nested collections, linked
        structures). Max depth: {maxDepth}.
      </p>
      <DepthHistogramChart data={hist} />
      <details>
        <summary>Full depth table ({fmtCount(hist.length)} buckets)</summary>
        <StdTable columns={depthCols} data={rows} searchKeys={[]} />
      </details>
    </section>
  );
}

// ── Leak Indicators ─────────────────────────────────────────────────────────
// Scalar signals for common Java leak patterns. Only rendered when at least
// one indicator is non-zero. Mirrors render_md.rs::render_leak_indicators.
function LeakIndicatorsSection({ data }: { data?: LeakIndicators }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  if (!data) return null;
  const { anonymous_class_count, thread_local_null_key_count, direct_byte_buffer_capacity_sum } = data;
  if (anonymous_class_count === 0 && thread_local_null_key_count === 0 && direct_byte_buffer_capacity_sum === 0) {
    return null;
  }
  return (
    <section id="leak-indicators">
      <h2>Leak Indicators</h2>
      <p className="subtitle">
        Scalar signals for common Java leak patterns. Non-zero values here are worth investigating.
      </p>
      {(() => {
        type LeakRow = { indicator: React.ReactNode; value: string };
        const leakRows: LeakRow[] = [
          ...(anonymous_class_count > 0 ? [{ indicator: "Anonymous/generated classes", value: fmtCount(anonymous_class_count) }] : []),
          ...(thread_local_null_key_count > 0 ? [{ indicator: <><code>ThreadLocal</code> null-key entries (cleared referent)</>, value: fmtCount(thread_local_null_key_count) }] : []),
          ...(direct_byte_buffer_capacity_sum > 0 ? [{ indicator: <><code>DirectByteBuffer</code> total capacity</>, value: fmtB(direct_byte_buffer_capacity_sum) }] : []),
        ];
        const leakCols: TableColumn<LeakRow>[] = [
          { id: "indicator", name: "Indicator", grow: 1, cell: (r) => <span>{r.indicator}</span> },
          { id: "value", name: "Value", right: true, width: "140px", selector: (r) => r.value },
        ];
        return <StdTable columns={leakCols} data={leakRows} searchKeys={[]} fmtBtn={kbBtn} />;
      })()}
    </section>
  );
}

// ── Top Retainers (§813) ───────────────────────────────────────────────────────
// Merged Class#field + stack-frame retainers, sorted by retained desc.
function TopRetainersSection({ rows }: { rows?: import("./types").RetainerRow[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  if (!rows || rows.length === 0) return null;
  return (
    <section id="top-retainers">
      <h2>Top Retainers</h2>
      <p className="subtitle">
        Merged ranking of <code>Class#field</code> references and stack-frame locals by total
        retained heap. Fields come from collection attribution; stack frames from thread-local
        analysis (<code>--thread-locals</code>).
      </p>
      {(() => {
        const retainerCols: TableColumn<import("./types").RetainerRow>[] = [
          { id: "name", name: "Name", grow: 1, cell: (r) => <code>{r.name}</code>, selector: (r) => r.name },
          { id: "kind", name: "Kind", width: "120px", selector: (r) => r.kind },
          { id: "retained", name: useKB ? "Retained (KB)" : "Retained", right: true, width: useKB ? "140px" : "120px", cell: byteCell(r => r.retained, fmtB, useKB), selector: (r) => r.retained },
        ];
        return <StdTable columns={retainerCols} data={rows} searchKeys={["name"]} fmtBtn={kbBtn} defaultSortFieldId="retained" />;
      })()}
    </section>
  );
}

// ── Glossary (end section, mirrors the Markdown glossary) ─────────────────────
// ── Custom Queries ───────────────────────────────────────────────────────────
// Renders report.queries (user-supplied OQL results). Query results are already
// LIMIT-capped server-side, so every row is rendered (no ShowMore). React
// escapes each {cell} text child automatically — no manual HTML escaping.

// Format a single QueryValue cell for display. Mirrors fmt_query_value in
// src/report/render_md.rs (ObjRef renders as `class@index`).
function fmtCell(v: QueryValue): string {
  switch (v.kind) {
    case "null":
      return "null";
    case "bool":
    case "int":
    case "float":
      return String(v.v);
    case "str":
      return v.v;
    case "obj_ref":
      return `${v.v.class}@${v.v.index}`;
  }
}

function CustomQueriesSection({ report }: { report: Report }) {
  const queries = report.queries;
  if (!queries?.length) return null;
  return (
    <section id="custom-queries">
      <h2>Custom Queries</h2>
      <p className="subtitle">Results of the OQL queries supplied on the command line.</p>
      {queries.map((q: QueryResult, qi) => (
        <div key={qi}>
          <h3>{q.name}</h3>
          <pre>{q.oql}</pre>
          {q.error ? (
            <p className="subtitle">
              <strong>Error:</strong> {q.error}
            </p>
          ) : (
            <>
              {(() => {
                const queryCols: TableColumn<QueryValue[]>[] = q.columns.map((c, ci) => ({
                  id: `col_${ci}`,
                  name: c.name,
                  grow: 1,
                  cell: (row) => {
                    const val = row[ci];
                    const text = fmtCell(val);
                    return val.kind === "str" ? <ExpandableText text={text} label={c.name} /> : <span>{text}</span>;
                  },
                  selector: (row) => fmtCell(row[ci]),
                }));
                return <StdTable columns={queryCols} data={q.rows} searchKeys={[]} cap={q.rows.length} />;
              })()}
              <p className="subtitle">
                {q.row_count} row(s){q.truncated ? ", truncated" : ""}
              </p>
              {q.note && <p className="subtitle">Note: {q.note}</p>}
              <QueryViz query={q} />
            </>
          )}
        </div>
      ))}
    </section>
  );
}

function GlossarySection() {
  const entries: [string, React.ReactNode][] = [
    ["Shallow size", <>the memory an object occupies by itself: its header plus its own fields (and, for an array, its elements). It does <em>not</em> include the objects it points to.</>],
    ["Retained heap (retained size)", <>the total memory that would be freed if this object were garbage-collected: its own shallow size plus everything reachable <em>only</em> through it. This is the basis for every percentage in this report. See <a href="https://en.wikipedia.org/wiki/Dominator_(graph_theory)" target="_blank" rel="noreferrer">dominator (graph theory)</a>.</>],
    ["Reachable heap", <>all objects the <a href="https://en.wikipedia.org/wiki/Garbage_collection_(computer_science)" target="_blank" rel="noreferrer">garbage collector</a> can still reach from a GC root. Anything unreachable is already collectible and is excluded from the totals here.</>],
    ["GC root", <>an object the JVM keeps alive unconditionally: live thread stacks (local variables), static fields of loaded classes, <a href="https://en.wikipedia.org/wiki/Java_Native_Interface" target="_blank" rel="noreferrer">JNI</a> references, and similar. Every retained-size chain ends at a GC root.</>],
    ["Dominator", <>object <em>A</em> dominates object <em>B</em> if every path from a GC root to <em>B</em> passes through <em>A</em>. An object's retained heap is exactly the set of objects it dominates. See <a href="https://en.wikipedia.org/wiki/Dominator_(graph_theory)" target="_blank" rel="noreferrer">dominator (graph theory)</a>.</>],
    ["Dominator tree", <>the tree formed by linking each object to its immediate dominator. Retained sizes are computed by summing shallow sizes up this tree.</>],
    ["Top-level dominator", <>an object whose immediate dominator is a GC root, so it sits at the top of the dominator tree. The "Biggest Objects" and "Retention Concentration" views rank these.</>],
    ["Dominator depth", <>how many dominator-tree hops an object sits below a GC root. Shallow depth means most objects are held close to a root; deep depth means retention flows through long chains.</>],
    ["Accumulation point", <>a single object (often a collection, cache, or map) that dominates a large number of instances of the <em>same</em> class, meaning where a <a href="https://en.wikipedia.org/wiki/Memory_leak" target="_blank" rel="noreferrer">memory leak</a> accumulates.</>],
    ["Class loader", <>the JVM component that defined a class. The same class name loaded by two different <a href="https://en.wikipedia.org/wiki/Java_Classloader" target="_blank" rel="noreferrer">class loaders</a> is two distinct classes in the heap, so heap is attributed per (class, loader) pair.</>],
    ["Referent", <>the object that a reference field points <em>to</em>. A <a href="https://en.wikipedia.org/wiki/Weak_reference" target="_blank" rel="noreferrer"><code>WeakReference</code></a>, for example, has a referent it does not keep alive.</>],
    ["Instance vs. class", <>an <em>instance</em> is one object; a <em>class</em> row aggregates every instance of that type. "Largest" in the histogram is the shallow size of the single biggest instance of a class.</>],
  ];
  return (
    <section id="glossary">
      <h2>Glossary</h2>
      <p className="subtitle">Definitions for the terms used above.</p>
      <dl className="summary-grid">
        {entries.map(([term, def]) => (
          <React.Fragment key={term}>
            <dt>{term}</dt>
            <dd>{def}</dd>
          </React.Fragment>
        ))}
      </dl>
    </section>
  );
}

// ── Cross-dump time-series diff view ─────────────────────────────────────────
// Renders a SeriesDiffResult: a legend (r1..rN → labels), headline totals, and
// one sortable N-column table per section. The HTML diff view embeds a tagged
// {"kind":"series-diff","diff":…} envelope in #report-data; index.tsx dispatches
// to this component when it sees that discriminator.

const MINUS = "−"; // typographic minus, matching the Markdown renderer.

// Signed byte delta, e.g. "+1.2 MB" / "−340 KB" / "0 B".
function fmtDeltaBytes(n: number, fmtB: (n: number) => string): string {
  if (n === 0) return "0 B";
  const sign = n > 0 ? "+" : MINUS;
  return sign + fmtB(Math.abs(n));
}

// Signed count delta with thousands separators, e.g. "+1,024" / "−17" / "0".
function fmtDeltaCount(n: number): string {
  if (n === 0) return "0";
  const sign = n > 0 ? "+" : MINUS;
  return sign + Math.abs(n).toLocaleString("en-US");
}

// A sortable, N-column class/suspect table. Columns: name | r1 … rN | Δ.
// Sorting is descending by the chosen numeric key: any per-report column
// (its retained value) or the Δ column. Copies before sorting so the model
// is never mutated.
function SeriesTable({
  nameLabel,
  labels,
  rows,
  showNew,
}: {
  nameLabel: string;
  labels: string[];
  rows: (SeriesClassRow | SeriesSuspectRow)[];
  showNew?: boolean;
}) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const n = labels.length;
  type SRow = SeriesClassRow | SeriesSuspectRow;
  const seriesCols: TableColumn<SRow>[] = [
    { id: "name", name: nameLabel, grow: 1, cell: (r) => <code>{r.pretty_class}</code>, selector: (r) => r.pretty_class, sortable: true },
    ...labels.map((lbl, i): TableColumn<SRow> => ({
      id: `r${i}`,
      name: useKB ? `r${i + 1} (KB)` : `r${i + 1}`,
      right: true,
      width: useKB ? "120px" : "100px",
      cell: (r) => byteCell((row: SRow) => row.retained[i] ?? 0, fmtB, useKB)(r),
      selector: (r) => r.retained[i] ?? 0,
      sortable: true,
    })),
    { id: "delta", name: "Δ(r1→rN)", right: true, width: "110px", cell: (r) => fmtDeltaBytes(r.delta_retained, fmtB), selector: (r) => r.delta_retained, sortable: true },
    ...(showNew ? [{
      id: "new",
      name: "New?",
      width: "60px",
      cell: (r: SRow) => ("is_new" in r && r.is_new ? "yes" : ""),
      selector: (r: SRow) => ("is_new" in r && r.is_new ? 1 : 0),
    } as TableColumn<SRow>] : []),
  ];
  return <StdTable columns={seriesCols} data={rows} searchKeys={["pretty_class"]} fmtBtn={kbBtn} defaultSortFieldId="delta" />;
}

// One diff section: a heading, and either the sortable table or an empty note.
function DiffSection({
  title,
  nameLabel,
  labels,
  rows,
  emptyNote,
  showNew,
}: {
  title: string;
  nameLabel: string;
  labels: string[];
  rows: (SeriesClassRow | SeriesSuspectRow)[];
  emptyNote: string;
  showNew?: boolean;
}) {
  return (
    <section className="diff-section">
      <h2>{title}</h2>
      {rows.length === 0 ? (
        <p>{emptyNote}</p>
      ) : (
        <SeriesTable nameLabel={nameLabel} labels={labels} rows={rows} showNew={showNew} />
      )}
    </section>
  );
}

// The verdict line: mirrors the Markdown verdict (the sole percentage).
function diffVerdict(diff: SeriesDiffResult, fmtB: (n: number) => string): string {
  const firstShallow = diff.total_shallow[0] ?? 0;
  const newSuspects = diff.grown_suspects.filter((s) => s.is_new).length;
  let line: string;
  if (firstShallow === 0) {
    // Undefined percentage against an empty baseline (§37.3).
    if (diff.delta_total_shallow > 0) {
      const lead = diff.growth_leaders[0];
      const driver = lead
        ? `; largest driver ${lead.pretty_class} (${fmtDeltaBytes(lead.delta_retained, fmtB)} retained)`
        : "";
      line = `Heap grew by ${fmtDeltaBytes(diff.delta_total_shallow, fmtB)} shallow (baseline was empty)${driver}.`;
    } else {
      line = "Heap size is unchanged (baseline was empty).";
    }
  } else {
    const pct = (diff.delta_total_shallow / firstShallow) * 100;
    if (diff.delta_total_shallow > 0) {
      const lead = diff.growth_leaders[0];
      const driver = lead
        ? `; largest driver ${lead.pretty_class} (${fmtDeltaBytes(lead.delta_retained, fmtB)} retained)`
        : "";
      line = `Heap grew ${pct.toFixed(1)}% (${fmtDeltaBytes(diff.delta_total_shallow, fmtB)} shallow)${driver}.`;
    } else if (diff.delta_total_shallow < 0) {
      line = `Heap shrank ${Math.abs(pct).toFixed(1)}% (${fmtDeltaBytes(diff.delta_total_shallow, fmtB)} shallow); no net growth.`;
    } else {
      line = "Heap size is unchanged.";
    }
  }
  // Gross churn when a net-flat/shrinking series still churned a lot (§37.2).
  if (
    diff.gross_growth_retained > 0 &&
    diff.gross_growth_retained > Math.max(diff.net_delta_retained, 0) * 2
  ) {
    line += ` Gross retained churn: +${fmtB(diff.gross_growth_retained)} grown / ${MINUS}${fmtB(diff.gross_shrink_retained)} reclaimed across steps.`;
  }
  if (newSuspects > 0) {
    line += ` ${newSuspects} new suspect${newSuspects === 1 ? "" : "s"}.`;
  }
  return line;
}

// A dedicated table for Transient Spikes (§37.1): name | r1…rN | Peak | Peak−r1.
function SpikeTable({ labels, rows }: { labels: string[]; rows: SeriesClassRow[] }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const spikeCols: TableColumn<SeriesClassRow>[] = [
    { id: "name", name: "Class", grow: 1, cell: (r) => <code>{r.pretty_class}</code>, selector: (r) => r.pretty_class, sortable: true },
    ...labels.map((lbl, i): TableColumn<SeriesClassRow> => ({
      id: `r${i}`,
      name: useKB ? `r${i + 1} (KB)` : `r${i + 1}`,
      right: true,
      width: useKB ? "120px" : "100px",
      cell: (r) => byteCell((row: SeriesClassRow) => row.retained[i] ?? 0, fmtB, useKB)(r),
      selector: (r) => r.retained[i] ?? 0,
      sortable: true,
    })),
    { id: "peak", name: useKB ? "Peak (KB)" : "Peak", right: true, width: useKB ? "120px" : "100px", cell: byteCell(r => r.peak_retained, fmtB, useKB), selector: (r) => r.peak_retained, sortable: true },
    { id: "peakOverBaseline", name: "Peak−r1", right: true, width: "110px", cell: (r) => fmtDeltaBytes(r.peak_over_baseline, fmtB), selector: (r) => r.peak_over_baseline, sortable: true },
  ];
  return <StdTable columns={spikeCols} data={rows} searchKeys={["pretty_class"]} fmtBtn={kbBtn} defaultSortFieldId="peak" />;
}

export function DiffApp({ diff }: { diff: SeriesDiffResult }) {
  const [fmtB, kbBtn, useKB] = useFmtBytes();
  const { labels } = diff;
  return (
    <div className="app">
      <h1>Heap Dump Comparison ({labels.length} reports)</h1>
      <p className="subtitle">
        Cross-dump growth across a time series (first = baseline, last = current).
      </p>
      <div className="theme-toggle-wrap">
        <ThemeToggle />
      </div>

      <section className="diff-section">
        <h2>Reports</h2>
        {kbBtn && <div className="tools">{kbBtn}</div>}
        <ol className="diff-legend">
          {labels.map((lbl, i) => (
            <li key={i}>
              <code>r{i + 1}</code> = {lbl}
            </li>
          ))}
        </ol>
      </section>

      <section className="diff-section">
        <h2>Headline Totals</h2>
        <p><strong>Verdict:</strong> {diffVerdict(diff, fmtB)}</p>
        <ul>
          <li><strong>Δ Objects (r1→rN):</strong> {fmtDeltaCount(diff.delta_total_objects)}</li>
          <li><strong>Δ Shallow heap (r1→rN):</strong> {fmtDeltaBytes(diff.delta_total_shallow, fmtB)}</li>
          <li><strong>Net Δ Retained (all classes, r1→rN):</strong> {fmtDeltaBytes(diff.net_delta_retained, fmtB)}</li>
          <li><strong>Gross Retained churn (all classes, per-step):</strong> +{fmtB(diff.gross_growth_retained)} grown / {MINUS}{fmtB(diff.gross_shrink_retained)} reclaimed</li>
        </ul>
      </section>

      <DiffSection
        title="Growth Leaders (by Δ retained)"
        nameLabel="Class"
        labels={labels}
        rows={diff.growth_leaders}
        emptyNote="No class grew in retained heap."
      />
      {diff.spike_leaders.length > 0 ? (
        <section className="diff-section">
          <h2>Transient Spikes (peak above baseline)</h2>
          <p>
            Classes that climbed well above their baseline mid-series then fell back — a
            first→last Δ alone would miss them. Ranked by peak-over-baseline; the peak may be
            at any intermediate dump.
          </p>
          <SpikeTable labels={labels} rows={diff.spike_leaders} />
        </section>
      ) : null}
      <DiffSection
        title="New Classes"
        nameLabel="Class"
        labels={labels}
        rows={diff.new_classes}
        emptyNote="No classes are new in the current dump."
      />
      <DiffSection
        title="Removed Classes"
        nameLabel="Class"
        labels={labels}
        rows={diff.removed_classes}
        emptyNote="No classes dropped out of the current dump."
      />
      <DiffSection
        title="New / Grown Leak Suspects"
        nameLabel="Suspect"
        labels={labels}
        rows={diff.grown_suspects}
        emptyNote="No leak suspect is new or grew in the current dump."
        showNew
      />
      <DiffSection
        title="Shrunk Leak Suspects"
        nameLabel="Suspect"
        labels={labels}
        rows={diff.shrunk_suspects}
        emptyNote="No leak suspect shrank in the current dump."
      />
      <section className="diff-section">
        <h2>Disappeared Leak Suspects (resolved)</h2>
        <p>
          Informational: these were flagged in an earlier dump but are gone from the current
          one — a fixed or transient issue, not a current problem. Listed last for that reason.
        </p>
        {diff.gone_suspects.length === 0 ? (
          <p>No leak suspect disappeared in the current dump.</p>
        ) : (
          <SeriesTable nameLabel="Suspect" labels={labels} rows={diff.gone_suspects} />
        )}
      </section>
      <BackToTop />
    </div>
  );
}

export default function App({ report }: { report: Report }) {
  const [expandAllTables, setExpandAllTables] = React.useState(false);

  // Scroll to the URL hash once the DOM has been painted after initial render.
  // The browser fires the native hash-scroll before React mounts, so we must
  // replay it here.
  React.useEffect(() => {
    const hash = window.location.hash.slice(1);
    if (!hash) return;
    requestAnimationFrame(() => {
      document.getElementById(hash)?.scrollIntoView({ behavior: "smooth" });
    });
  }, []); // empty deps → runs once after first render

  return (
    <TableExpansionCtx.Provider value={expandAllTables}>
    <div className="app">
      <a href="#memory-triage" className="skip-link">Skip to content</a>
      <h1>
        Heap Dump Analysis:{" "}
        <span className="copy-cell">
          <code>{report.overview?.source_name ?? "(unknown)"}</code>
          <CopyBtn text={report.overview?.source_name ?? ""} />
        </span>
      </h1>
      <p className="subtitle" style={{ marginTop: "-0.5rem" }}>
        {report.overview?.dump_creation != null
          ? <>{formatEpochMs(report.overview.dump_creation)} · </>
          : null}
        All sizes are binary (1&nbsp;KB = 1024 bytes, 1&nbsp;MB = 1024&nbsp;KB, and so on).
      </p>
      <div className="theme-toggle-wrap">
        <button className="theme-toggle" onClick={() => setExpandAllTables((v) => !v)}>
          {expandAllTables ? "⊟ Collapse tables" : "⊞ Expand all tables"}
        </button>
        <ThemeToggle />
      </div>
      <Nav report={report} />
      <OomTriage report={report} />
      <WasteSummarySection report={report} />
      <KpiStrip report={report} />
      <SystemOverviewSection report={report} />
      <RecordCensusSection report={report} />
      <LeakSuspectsSection report={report} />
      <TopConsumersSection report={report} />
      <SizeDistributionSection report={report} />
      <DuplicateStringsSection report={report} />
      <DuplicatePrimArraysSection report={report} />
      <BoxedNumbersSection report={report} />
      <HeaderOverheadSection report={report} />
      <DominatorAnalysisSection data={report.dominator_analysis} />
      <ThreadsSection report={report} />
      {report.top_components?.components?.length ? (
        <TopComponentsSection data={report.top_components} />
      ) : null}
      <ArraysBySizeSection data={report.arrays_by_size} totalShallow={report.overview.total_shallow} />
      <CollectionsSection data={report.collections} />
      {report.collection_attribution && (
        <CollectionAttributionSection data={report.collection_attribution} />
      )}
      {report.fields_by_size && <FieldsBySizeSection data={report.fields_by_size} />}
      {report.top_retainers && report.top_retainers.length > 0 && (
        <TopRetainersSection rows={report.top_retainers} />
      )}
      {report.biggest_collections && <BiggestCollectionsSection data={report.biggest_collections} />}
      {report.collection_contents && <CollectionContentsSection data={report.collection_contents} />}
      <ReferencesSection data={report.references} />
      <UnreachableObjectsSection data={report.overview} />
      {report.alloc_sites && <AllocSitesSection data={report.alloc_sites} />}
      <RetentionConcentrationSection report={report} />
      <DominatorDepthSection report={report} />
      <LeakIndicatorsSection data={report.leak_indicators} />
      <CustomQueriesSection report={report} />
      <GlossarySection />
      <BackToTop />
    </div>
    </TableExpansionCtx.Provider>
  );
}

/// Catches any render-time exception below it and shows a styled panel instead of
/// a blank page. `boot()` wraps the whole app in this, so a bug in one section
/// degrades to an error message rather than a white screen with the report data
/// still embedded but invisible.
export class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { error: null };
  }
  static getDerivedStateFromError(error: Error) {
    return { error };
  }
  componentDidCatch(error: Error, info: React.ErrorInfo) {
    // Surface to the console for anyone with devtools open.
    console.error("hprof-analyzer report render failed:", error, info);
  }
  render() {
    if (this.state.error) {
      return (
        <div className="render-error" role="alert">
          <h1>Report failed to render</h1>
          <p>
            The report data loaded, but a rendering error occurred. This is a bug
            in the viewer — the underlying JSON is intact. Details:
          </p>
          <pre>{String(this.state.error?.stack || this.state.error)}</pre>
        </div>
      );
    }
    return this.props.children;
  }
}
