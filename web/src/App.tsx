import React from "react";
import type { AllocSites, ArraysBySize, ClassRow, CollectionAttribution, CollectionsAnalysis, Component, DomTreeNode, DominatorAnalysis, FillRatioBucket, HistRow, LeakIndicators, MergedPathNode, ObjRow, PackageNode, ReferencesAnalysis, ReferenceStats, RefStatClassRow, Report, RootPathStep, SeriesClassRow, SeriesDiffResult, SeriesSuspectRow, Suspect, SystemOverview, ThreadInfo, ThreadLocalObj, TopArrays, TopComponents, UnreachableClassRow } from "./types";
import { fmtCount, fmtExactBytes, formatBytes, formatEpochMs, pctOf, shortLoader } from "./format";
import {
  CompositionStackedBar,
  ConcentrationChart,
  ConcentrationStackedBar,
  DepthHistogramChart,
  GcRootsChart,
  HeapCompositionChart,
  LeakShareChart,
  LoaderRollupChart,
  TopClassesChart,
  TreemapBar,
} from "./charts";

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

// ── Navigation ───────────────────────────────────────────────────────────────
// A sticky in-page table of contents so long reports (hundreds of threads,
// thousands of histogram rows) stay navigable — MAT's report has an equivalent
// left-hand section index.
function Nav({ report }: { report: Report }) {
  // [id, label, group?] — group is set only on the first link of each group.
  const items: [string, string, string?][] = [];

  // ── Overview group ──
  items.push(
    ["triage",        "OOM Triage",          "Overview"],
    ["overview",      "System Overview"],
    ["record-census", "HPROF Record Census"],
  );

  // ── Analysis group ──
  items.push(["leaks",              "Leak Suspects",    "Analysis"]);
  items.push(["top",                "Top Consumers"]);
  items.push(["dominator-analysis", "Dominator Analysis"]);
  items.push(["threads",            "Threads"]);
  if (report.top.size_distribution.count > 0) items.push(["size-distribution", "Size Distribution"]);

  // ── Data group ──
  let dataGroupSet = false;
  const addData = (id: string, label: string) => {
    if (!dataGroupSet) { items.push([id, label, "Data"]); dataGroupSet = true; }
    else items.push([id, label]);
  };
  if (report.overview.duplicate_strings) addData("duplicate-strings", "Duplicate Strings");
  if (report.top_components?.components?.length) addData("top-components", "Top Components");
  addData("arrays-by-size", "Arrays by Size");
  addData("collections", "Collections");
  if (report.collection_attribution) addData("container-attribution", "Container Attribution");
  addData("references", "References");
  addData("unreachable-objects", "Unreachable Objects");
  if (report.alloc_sites) addData("alloc-sites", "Allocation Sites");

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
      {items.map(([id, label, group]) => (
        <React.Fragment key={id}>
          {group && <span className="toc-group">{group}</span>}
          <a href={`#${id}`} className={id === active ? "active" : ""}>{label}</a>
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
// Re-projects already-modeled fields (matches render_markdown's render_oom_triage).
function OomTriage({ report }: { report: Report }) {
  const total = report.leaks.total_shallow;
  const lines: React.ReactNode[] = [];

  const first = report.leaks.suspects[0];
  if (first) {
    const kind = first.is_single ? "a single object" : "a class group";
    lines.push(
      <>
        <strong>Headline retainer:</strong> <code>{first.pretty_class}</code> ({kind}) retains{" "}
        {formatBytes(first.retained)} ({pctOf(first.retained, total).toFixed(1)}% of reachable heap).
      </>,
    );
  } else if (report.top.biggest_objects[0]) {
    const o = report.top.biggest_objects[0];
    lines.push(
      <>
        <strong>Headline retainer:</strong> <code>{o.display_class}</code> retains{" "}
        {formatBytes(o.retained)} ({pctOf(o.retained, total).toFixed(1)}% of reachable heap).
      </>,
    );
  } else {
    lines.push(
      <>
        <strong>Headline retainer:</strong> No dominant retainer found.
      </>,
    );
  }

  if (first && pctOf(first.retained, total) >= 50) {
    lines.push(
      <>
        <strong>Concentration:</strong> A single object/class group dominates the heap (
        {pctOf(first.retained, total).toFixed(1)}%).
      </>,
    );
  } else if (first) {
    lines.push(
      <>
        <strong>Concentration:</strong> Retention is spread across multiple roots.
      </>,
    );
  } else {
    lines.push(
      <>
        <strong>Concentration:</strong> No suspect exceeds the threshold; retention is spread across many roots.
      </>,
    );
  }

  const hist = report.overview.dominator_depth_histogram;
  if (hist.length > 0) {
    const totObj = hist.reduce((s, b) => s + b.objects, 0);
    const maxDepth = hist.reduce((m, b) => Math.max(m, b.depth), 0);
    let cum = 0;
    let p90 = maxDepth;
    for (const b of hist) {
      cum += b.objects;
      if (cum * 10 >= totObj * 9) {
        p90 = b.depth;
        break;
      }
    }
    const shape =
      p90 <= 3
        ? "shallow (most objects are held within a few hops of a GC root)"
        : "deep (retention flows through long dominator chains — often nested collections or linked structures)";
    lines.push(
      <>
        <strong>Shape:</strong> {shape} — 90% of objects within depth {p90}, max depth {maxDepth}.
      </>,
    );
  }

  const rc = report.overview.retention_concentration;
  if (rc.top1_bp > 0 || rc.num_objects_ge_1pct > 0) {
    lines.push(
      <>
        <strong>One leak or many:</strong> the single biggest object retains {(rc.top1_bp / 100).toFixed(1)}% and the
        top 10 retain {(rc.top10_bp / 100).toFixed(1)}% of the heap; {fmtCount(rc.num_objects_ge_1pct)} object(s) each
        hold ≥1%.
      </>,
    );
  }

  return (
    <div className="oom" id="triage" tabIndex={-1}>
      <h2>OOM Triage</h2>
      <p className="subtitle">Where the reachable heap is concentrated, at a glance.</p>
      <ul>
        {lines.map((l, i) => (
          <li key={i}>{l}</li>
        ))}
      </ul>
    </div>
  );
}

// ── KPI card strip ──────────────────────────────────────────────────────────
function KpiStrip({ report }: { report: Report }) {
  const suspects = report.leaks.suspects;
  const top = suspects[0];
  const topShare = top
    ? pctOf(top.retained, report.leaks.total_shallow).toFixed(1) + "%"
    : "—";
  const dominantClass = top?.pretty_class ?? "—";

  // Plain-language verdict mirroring the Markdown executive summary
  // ("Likely problem:" line). CONCENTRATION_PCT = 50.
  const pct = top ? pctOf(top.retained, report.leaks.total_shallow) : 0;
  let verdict: React.ReactNode;
  if (top && pct >= 50) {
    verdict = (
      <>
        <strong>Likely problem:</strong> <code>{top.pretty_class}</code> retains {pct.toFixed(1)}% of the reachable heap
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
      <div className="kpi">
        <div className="kpi-value">{formatBytes(report.overview.total_shallow)}</div>
        <div className="kpi-label">Total heap</div>
      </div>
      <div className="kpi">
        <div className="kpi-value">{fmtCount(report.overview.total_objects)}</div>
        <div className="kpi-label">Objects</div>
      </div>
      <div className="kpi">
        <div className="kpi-value">{fmtCount(suspects.length)}</div>
        <div className="kpi-label">Leak suspects</div>
      </div>
      <div className="kpi">
        <div className="kpi-value">{topShare}</div>
        <div className="kpi-label">Top suspect share</div>
      </div>
      <div className="kpi">
        <div className="kpi-value">
          <code title={dominantClass}>{dominantClass}</code>
        </div>
        <div className="kpi-label">Dominant retainer</div>
      </div>
      <div className="kpi">
        <div className="kpi-value">{fmtCount(report.overview.gc_roots)}</div>
        <div className="kpi-label">GC roots</div>
      </div>
      </div>
      <p className="subtitle" style={{ fontSize: "1rem" }}>{verdict}</p>
    </>
  );
}

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
  return (
    <th className={"num sortable" + (active ? " active" : "")} onClick={() => setSortKey(colKey)} title={`Sort by ${label} (descending)`}>
      {label} {active ? "▾" : ""}
    </th>
  );
}

// ── Sortable / filterable class histogram ────────────────────────────────────
type HistKey = "instances" | "shallow" | "max_instance_shallow" | "retained";
const HIST_COLS: { key: HistKey; label: string }[] = [
  { key: "instances", label: "Instances" },
  { key: "shallow", label: "Shallow" },
  { key: "max_instance_shallow", label: "Largest" },
  { key: "retained", label: "Retained" },
];

function ClassHistogramTable({ rows }: { rows: HistRow[] }) {
  const [sortKey, setSortKey] = React.useState<HistKey>("retained");
  const [filter, setFilter] = React.useState("");

  // Show the class-loader column only when at least one class was loaded by a
  // non-boot loader — otherwise every cell would read "<boot>" and add noise.
  const showLoader = React.useMemo(
    () => rows.some((r) => r.loader_label != null && r.loader_label !== "<boot>"),
    [rows],
  );

  const view = React.useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const filtered = needle
      ? rows.filter((r) => r.pretty_class.toLowerCase().includes(needle))
      : rows;
    // Stable sort descending by the chosen numeric column. Copy first so we
    // never mutate the report model.
    return [...filtered].sort((a, b) => b[sortKey] - a[sortKey]);
  }, [rows, sortKey, filter]);

  const CAP = 500;
  const shown = view.slice(0, CAP);

  return (
    <details>
      <summary>Show full class histogram ({fmtCount(rows.length)} rows)</summary>
      <div className="tools">
        <input
          type="text"
          className="filter"
          placeholder="Filter by class name…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          aria-label="Filter histogram by class name"
        />
        <span className="hint">
          {fmtCount(view.length)} match{view.length === 1 ? "" : "es"}
          {view.length > CAP ? ` (showing first ${CAP})` : ""} — click a column to sort
        </span>
      </div>
      <table>
        <thead>
          <tr>
            <th>#</th>
            <th>Class</th>
            {showLoader && <th>Loader</th>}
            {HIST_COLS.map((c) => (
              <SortableTh<HistRow> key={c.key} label={c.label} colKey={c.key} sortKey={sortKey} setSortKey={setSortKey} />
            ))}
          </tr>
        </thead>
        <tbody>
          {shown.map((h, i) => (
            <tr key={i}>
              <td className="num">{i + 1}</td>
              <td>
                <span className="copy-cell">
                  <code>{h.pretty_class}</code>
                  <CopyBtn text={h.pretty_class} />
                </span>
              </td>
              {showLoader && (
                <td>
                  <LoaderCell label={h.loader_label} />
                </td>
              )}
              <td className="num">{fmtCount(h.instances)}</td>
              <td className="num">{formatBytes(h.shallow)}</td>
              <td className="num">{formatBytes(h.max_instance_shallow)}</td>
              <td className="num">{formatBytes(h.retained)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </details>
  );
}

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
  const rows: [string, number][] = [
    ["UTF8 strings", c.utf8_records],
    ["Load class", c.load_class_records],
    ["Unload class", c.unload_class_records],
    ["Stack frames", c.stack_frame_records],
    ["Stack traces", c.stack_trace_records],
    ["Heap dump segments", c.heap_dump_segments],
    ["Instance dumps", c.instance_dumps],
    ["Object-array dumps", c.obj_array_dumps],
    ["Primitive-array dumps", c.prim_array_dumps],
    ["Class dumps", c.class_dumps],
  ];
  return (
    <section id="record-census">
      <h2>HPROF Record Census</h2>
      <p className="subtitle">
        Raw HPROF record-type composition of the dump (pass-1 counts); additive, not parity-compared.
      </p>
      <table>
        <thead>
          <tr>
            <th>Record Type</th>
            <th className="num">Count</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(([label, count], i) => (
            <tr key={i}>
              <td>{label}</td>
              <td className="num">{fmtCount(count)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {c.gc_root_tag_counts.length > 0 && (
        <>
          <h3>GC Root Records by Tag</h3>
          <table>
            <thead>
              <tr>
                <th>Root Tag</th>
                <th className="num">Count</th>
              </tr>
            </thead>
            <tbody>
              {c.gc_root_tag_counts.map(([tag, count], i) => (
                <tr key={i}>
                  <td>{gcRootTagLabel(tag)}</td>
                  <td className="num">{fmtCount(count)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}
    </section>
  );
}

// ── Top-Dominator Size Distribution ───────────────────────────────────────────
function SizeDistributionSection({ report }: { report: Report }) {
  const d = report.top.size_distribution;
  if (d.count <= 0) return null;
  return (
    <section id="size-distribution">
      <h2>Top-Dominator Size Distribution</h2>
      <p className="subtitle">
        Retained-size spread across all {fmtCount(d.count)} top-level dominators (the biggest memory contributors).
      </p>
      <ul>
        <li>Dominators: {fmtCount(d.count)}</li>
        <li>Smallest / largest retained: {formatBytes(d.min)} / {formatBytes(d.max)}</li>
        <li>Median retained: {formatBytes(d.median)}</li>
        <li>Total retained (top-level): {formatBytes(d.total)}</li>
      </ul>
      <table>
        <thead>
          <tr>
            <th className="num">Size ≤</th>
            <th className="num">Count</th>
          </tr>
        </thead>
        <tbody>
          {d.buckets.map((b, i) => (
            <tr key={i}>
              <td className="num">{formatBytes(b.upper_bytes)}</td>
              <td className="num">{fmtCount(b.count)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

// ── Duplicate Strings (approximate) ────────────────────────────────────────────
function DuplicateStringsSection({ report }: { report: Report }) {
  const d = report.overview.duplicate_strings;
  if (!d) {
    return (
      <section id="duplicate-strings">
        <h2>Duplicate Strings (approximate)</h2>
        <p className="subtitle">
          Duplicate-string analysis not run (pass <code>--dup-strings</code>).
        </p>
      </section>
    );
  }
  const w = d.char_array_waste;
  return (
    <section id="duplicate-strings">
      <h2>Duplicate Strings (approximate)</h2>
      <p className="subtitle">
        Opt-in (--dup-strings): each java.lang.String value hashed to 64 bits; collisions accepted as approximation.
      </p>
      <ul>
        <li>Total String instances: {fmtCount(d.total_string_instances)}</li>
        <li>Distinct values: {fmtCount(d.distinct_values)}</li>
        <li>Duplicated values: {fmtCount(d.duplicated_values)}</li>
        <li>Approx wasted bytes: {formatBytes(d.approx_wasted_bytes)}</li>
      </ul>

      {d.top_duplicated.length > 0 && (
        <>
          <h3>Most-Duplicated Values</h3>
          <table>
            <thead>
              <tr>
                <th className="num">#</th>
                <th className="num">Count</th>
                <th className="num">Wasted</th>
                <th>Value</th>
              </tr>
            </thead>
            <tbody>
              {d.top_duplicated.map((s, i) => (
                <tr key={i}>
                  <td className="num">{i + 1}</td>
                  <td className="num">{fmtCount(s.count)}</td>
                  <td className="num">{formatBytes(s.wasted_bytes)}</td>
                  <td><code>{s.text}</code></td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {d.top_by_length.length > 0 && (
        <>
          <h3>Longest Values</h3>
          <table>
            <thead>
              <tr>
                <th className="num">#</th>
                <th className="num">Length</th>
                <th className="num">Count</th>
                <th>Value</th>
              </tr>
            </thead>
            <tbody>
              {d.top_by_length.map((s, i) => (
                <tr key={i}>
                  <td className="num">{i + 1}</td>
                  <td className="num">{fmtCount(s.len)}</td>
                  <td className="num">{fmtCount(s.count)}</td>
                  <td><code>{s.text}</code></td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {d.length_histogram.length > 0 && (
        <>
          <h3>String Length Distribution</h3>
          <p className="subtitle">
            Distinct-value lengths (bytes): min {fmtCount(d.length_stats.min)}, median {fmtCount(d.length_stats.median)},
            max {fmtCount(d.length_stats.max)}; total {formatBytes(d.length_stats.total)}.
          </p>
          <table>
            <thead>
              <tr>
                <th className="num">Length ≤</th>
                <th className="num">Values</th>
              </tr>
            </thead>
            <tbody>
              {d.length_histogram.map((b, i) => (
                <tr key={i}>
                  <td className="num">{fmtCount(b.upper_len)}</td>
                  <td className="num">{fmtCount(b.count)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {d.top_string_holders.length > 0 && (
        <>
          <h3>Classes Holding the Most Strings</h3>
          <p className="subtitle">
            Number of java.lang.String instances referenced by each class's instances.
          </p>
          <table>
            <thead>
              <tr>
                <th>Class</th>
                <th className="num">String refs</th>
              </tr>
            </thead>
            <tbody>
              {d.top_string_holders.map((h, i) => (
                <tr key={i}>
                  <td><code>{h.class_name}</code></td>
                  <td className="num">{fmtCount(h.string_refs)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {w && (
        <>
          <h3>Char[] Waste</h3>
          <p className="subtitle">
            {fmtCount(w.arrays_examined)} arrays examined, {fmtCount(w.wasteful_arrays)} wasteful,{" "}
            {formatBytes(w.total_wasted_bytes)} total wasted.
          </p>
          {w.top.length > 0 && (
            <table>
              <thead>
                <tr>
                  <th className="num">Array #</th>
                  <th className="num">Length</th>
                  <th className="num">Used</th>
                  <th className="num">Wasted</th>
                </tr>
              </thead>
              <tbody>
                {w.top.map((r, i) => (
                  <tr key={i}>
                    <td className="num">{fmtCount(r.array_obj_1based)}</td>
                    <td className="num">{fmtCount(r.length)}</td>
                    <td className="num">{formatBytes(r.used)}</td>
                    <td className="num">{formatBytes(r.wasted_bytes)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </section>
  );
}

// ── System Overview ─────────────────────────────────────────────────────────
function SystemOverviewSection({ report }: { report: Report }) {
  const o = report.overview;
  const threadCount = report.threads?.threads?.length ?? 0;
  return (
    <section id="overview">
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
          <dd>{formatBytes(o.file_size)}</dd>
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
          <dt>Total shallow heap</dt>
          <dd>{formatBytes(o.total_shallow)}</dd>
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
                {fmtCount(o.unreachable_count)} ({formatBytes(o.unreachable_shallow)})
              </dd>
            </>
          )}
          {(o.heap_fragmentation_ratio ?? 0) > 0 && (
            <>
              <dt>Heap fragmentation</dt>
              <dd>{((o.heap_fragmentation_ratio ?? 0) * 100).toFixed(1)}%</dd>
            </>
          )}
          {(o.top_class_concentration_bp ?? 0) > 0 && (
            <>
              <dt>Top-class retained concentration</dt>
              <dd>{((o.top_class_concentration_bp ?? 0) / 100).toFixed(1)}%</dd>
            </>
          )}
        </dl>
      </div>

      {o.system_properties.length > 0 ? (
        <details>
          <summary>System properties ({fmtCount(o.system_properties.length)})</summary>
          <div className="sysprops">
            <table>
              <thead>
                <tr>
                  <th>Key</th>
                  <th>Value</th>
                </tr>
              </thead>
              <tbody>
                {o.system_properties.map((p, i) => (
                  <tr key={i}>
                    <td>
                      <code>{p.key}</code>
                    </td>
                    <td className="sysprop-val">{p.value}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </details>
      ) : (
        <p className="subtitle">System properties not captured in this dump.</p>
      )}

      {o.heap_composition.by_kind.length > 0 && (
        <>
          <h3>Heap Composition</h3>
          <ChartOrNote hasData={o.heap_composition.by_kind.length >= 2} note="Composition chart needs at least two kinds; showing the table only.">
            <HeapCompositionChart data={o.heap_composition.by_kind} />
            <CompositionStackedBar data={o.heap_composition.by_kind} />
          </ChartOrNote>
          <table>
            <thead>
              <tr>
                <th>Kind</th>
                <th className="num">Objects</th>
                <th className="num">Shallow Heap</th>
              </tr>
            </thead>
            <tbody>
              {o.heap_composition.by_kind.map((k, i) => (
                <tr key={i}>
                  <td>{k.kind}</td>
                  <td className="num">{fmtCount(k.objects)}</td>
                  <td className="num">{formatBytes(k.shallow_heap)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {o.gc_roots_by_type.length > 0 && (
        <>
          <h3>GC Roots by Type</h3>
          <ChartOrNote hasData={o.gc_roots_by_type.length >= 2} note="Too few root types to chart; showing the table only.">
            <GcRootsChart data={o.gc_roots_by_type} />
          </ChartOrNote>
          <table>
            <thead>
              <tr>
                <th>Root Type</th>
                <th className="num">Count</th>
              </tr>
            </thead>
            <tbody>
              {o.gc_roots_by_type.map((r, i) => (
                <tr key={i}>
                  <td>{r.root_type}</td>
                  <td className="num">{fmtCount(r.count)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      <h3>Class Histogram (by Retained Heap)</h3>
      {o.histogram_truncated_to != null && (
        <p className="subtitle">
          Histogram capped to the largest {fmtCount(o.histogram_truncated_to)} classes.
        </p>
      )}
      <ChartOrNote hasData={o.histogram.length > 0} note="No histogram classes to chart.">
        <TopClassesChart data={o.histogram} />
      </ChartOrNote>
      <ClassHistogramTable rows={o.histogram} />

      {o.loader_rollup.length > 0 && (
        <>
          <h3>Class Loaders</h3>
          <p className="subtitle">
            Classes grouped by the loader that defined them. Many loaders each holding heap — especially the same class
            name under several loaders — can signal a class-loader leak.
          </p>
          <LoaderRollupChart data={o.loader_rollup} />
          <table>
            <thead>
              <tr>
                <th>Loader</th>
                <th className="num">Classes</th>
                <th className="num">Instances</th>
                <th className="num">Shallow</th>
                <th className="num">Retained</th>
              </tr>
            </thead>
            <tbody>
              {o.loader_rollup.map((r, i) => (
                <tr key={i}>
                  <td>{r.loader_label ?? `loader@${r.loader_id}`}</td>
                  <td className="num">{fmtCount(r.class_count)}</td>
                  <td className="num">{fmtCount(r.instances)}</td>
                  <td className="num">{formatBytes(r.shallow)}</td>
                  <td className="num">{formatBytes(r.retained)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {o.duplicate_classes.length > 0 && (
        <>
          <h3>Duplicate Classes</h3>
          <p className="subtitle">
            Class names loaded by more than one class loader — a classic class-loader-leak signature (the same class
            re-loaded repeatedly, e.g. per web-app or plugin reload).
          </p>
          <table>
            <thead>
              <tr>
                <th>Class</th>
                <th className="num">#Loaders</th>
                <th className="num">Instances</th>
                <th className="num">Retained</th>
              </tr>
            </thead>
            <tbody>
              {o.duplicate_classes.map((d, i) => (
                <React.Fragment key={i}>
                  <tr>
                    <td title={d.loaders.join(", ")}>
                      {d.per_loader && d.per_loader.length > 0 ? (
                        <details>
                          <summary>
                            <code>{d.pretty_class}</code>
                          </summary>
                          <table>
                            <thead>
                              <tr>
                                <th>Loader</th>
                                <th className="num">Instances</th>
                                <th className="num">Shallow</th>
                                <th className="num">Retained</th>
                              </tr>
                            </thead>
                            <tbody>
                              {d.per_loader.map((pl, j) => (
                                <tr key={j}>
                                  <td>
                                    <code>{pl.loader_label}</code>
                                  </td>
                                  <td className="num">{fmtCount(pl.instances)}</td>
                                  <td className="num">{formatBytes(pl.shallow)}</td>
                                  <td className="num">{formatBytes(pl.retained)}</td>
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        </details>
                      ) : (
                        <code>{d.pretty_class}</code>
                      )}
                    </td>
                    <td className="num">{fmtCount(d.loader_count)}</td>
                    <td className="num">{fmtCount(d.total_instances)}</td>
                    <td className="num">{formatBytes(d.total_retained)}</td>
                  </tr>
                </React.Fragment>
              ))}
            </tbody>
          </table>
        </>
      )}
    </section>
  );
}

// ── Leak Suspects ───────────────────────────────────────────────────────────
// Renders the accumulation "shortest path" (MAT's signature view) plus the
// per-class breakdown of what piles up at the accumulation point.
function AccumulationPath({ s }: { s: Suspect }) {
  if (s.path.length === 0) return null;
  return (
    <details open>
      <summary>Shortest path to the accumulation point ({s.path.length} steps)</summary>
      <ol className="accum-path">
        {s.path.map((p, i) => (
          <li key={i}>
            <code>{p.display_class}</code>{" "}
            <span className="path-ret">retains {formatBytes(p.retained)}</span>
          </li>
        ))}
      </ol>
    </details>
  );
}

function DominatedByClass({ rows }: { rows: HistRow[] }) {
  if (rows.length === 0) return null;
  return (
    <details>
      <summary>Accumulated objects grouped by class ({rows.length})</summary>
      <table>
        <thead>
          <tr>
            <th>Class</th>
            <th className="num">Instances</th>
            <th className="num">Shallow</th>
            <th className="num">Retained</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={i}>
              <td>
                <code>{r.pretty_class}</code>
              </td>
              <td className="num">{fmtCount(r.instances)}</td>
              <td className="num">{formatBytes(r.shallow)}</td>
              <td className="num">{formatBytes(r.retained)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </details>
  );
}

// the dominator chain from a suspect
// (first) up to its GC root (last), as a numbered list. The final step is annotated
// with the GC-root type when known. Mirrors report.rs::render_root_path.
function RootPathList({ steps }: { steps: RootPathStep[] }) {
  if (steps.length === 0) return null;
  const last = steps.length - 1;
  return (
    <details>
      <summary>Path to GC root · dominator chain ({steps.length} step{steps.length === 1 ? "" : "s"})</summary>
      <ol className="accum-path">
        {steps.map((p, i) => (
          <li key={i}>
            <code>{p.display_class}</code>{" "}
            <span className="path-ret">retains {formatBytes(p.retained)}</span>
            {i === last && p.root_type_label && (
              <> — <strong>GC root: {p.root_type_label}</strong></>
            )}
          </li>
        ))}
      </ol>
    </details>
  );
}

// One node of the recursive dominator subtree, as a
// collapsible <details>/<summary> tree (modeled on PackageTreeRow). Children are
// rendered nested; leaves are non-collapsible. Mirrors report.rs::render_dom_tree.
function DomSubtreeNode({ node, depth }: { node: DomTreeNode; depth: number }) {
  const hasChildren = node.children.length > 0;
  const label = (
    <>
      <code>{node.display_class}</code>{" "}
      <span className="path-ret">
        shallow {formatBytes(node.shallow)} · retained {formatBytes(node.retained)}
      </span>
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
            <DomSubtreeNode key={i} node={c} depth={depth + 1} />
          ))}
        </ul>
      </details>
    </li>
  );
}

function DomSubtree({ node }: { node: DomTreeNode }) {
  return (
    <details>
      <summary>Dominator subtree</summary>
      <ul className="dom-subtree">
        <DomSubtreeNode node={node} depth={0} />
      </ul>
    </details>
  );
}

// One node of the recursive "merged shortest paths to GC roots" prefix tree
// (class-group suspects). Mirrors DomSubtreeNode. Each node shows the class, how
// many member chains pass through it, and the aggregate retained heap; a
// terminal GC-root node carries its root-type label.
function MergedPathsNode({ node, depth }: { node: MergedPathNode; depth: number }) {
  const hasChildren = node.children.length > 0;
  const label = (
    <>
      <code>{node.display_class}</code>{" "}
      <span className="path-ret">
        {fmtCount(node.object_count)} object{node.object_count === 1 ? "" : "s"} · retained {formatBytes(node.retained)}
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
  const share = pctOf(s.retained, total);
  return (
    <div className="suspect" id={`suspect-${rank}`}>
      <h3 style={{ margin: "0 0 0.25rem" }}>
        <span className="rank">Problem Suspect {rank}</span> <code>{s.pretty_class}</code>
        <span className="pill">{s.is_single ? "single object" : `class group ×${fmtCount(s.instance_count)}`}</span>
      </h3>
      <p style={{ margin: "0.25rem 0" }}>
        Retains <strong title={fmtExactBytes(s.retained)}>{formatBytes(s.retained)}</strong>{" "}
        <span className="mat-exact">
          {fmtExactBytes(s.retained)} ({share.toFixed(2)}%)
        </span>
        {s.shallow > 0 && <> · shallow {formatBytes(s.shallow)}</>}.
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
          {s.accumulation_retained != null && <> retaining {formatBytes(s.accumulation_retained)}</>}.
        </p>
      )}
      <AccumulationPath s={s} />
      <DominatedByClass rows={s.dominated_by_class} />
      {s.dominated.length > 0 && (
        <details>
          <summary>
            Accumulated objects in dominator tree{" "}
            {s.dominated_total_count > s.dominated_shown
              ? `(directly dominates ${fmtCount(s.dominated_total_count)}, showing top ${fmtCount(s.dominated_shown)})`
              : `(directly dominates ${fmtCount(s.dominated_total_count)})`}
          </summary>
          <table>
            <thead>
              <tr>
                <th>Class</th>
                <th className="num">Shallow</th>
                <th className="num">Retained</th>
              </tr>
            </thead>
            <tbody>
              {s.dominated.map((d, i) => (
                <tr key={i}>
                  <td>
                    <code>{d.display_class}</code>
                  </td>
                  <td className="num">{formatBytes(d.shallow)}</td>
                  <td className="num">{formatBytes(d.retained)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </details>
      )}
      {s.root_path && <RootPathList steps={s.root_path} />}
      {s.dominator_tree && <DomSubtree node={s.dominator_tree} />}
      {!s.is_single && s.merged_paths && <MergedPaths node={s.merged_paths} />}
    </div>
  );
}

function LeakSuspectsSection({ report }: { report: Report }) {
  const l = report.leaks;
  return (
    <section id="leaks">
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
            <LeakShareChart suspects={l.suspects} total={l.total_shallow} />
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
        <td className="num">{formatBytes(node.shallow_heap)}</td>
        <td className="num bar-cell">
          <span className="bar-bg">
            <span className="bar-fill" style={{ width: `${pct}%` }} />
          </span>
          {formatBytes(node.retained_heap)}
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
  const t = report.top;
  const total = report.leaks.total_shallow;
  const pkgRoot = t.biggest_packages;
  const maxPkgRetained = pkgRoot.children.reduce((m, c) => Math.max(m, c.retained_heap), 0);

  const objSort = useSortedRows<ObjRow>(t.biggest_objects, "retained");
  const clsSort = useSortedRows<ClassRow>(t.biggest_classes, "retained");

  return (
    <section id="top">
      <h2>Top Consumers</h2>
      <p className="subtitle">Biggest individual objects, classes, and packages by retained heap.</p>

      <h3>Biggest Objects</h3>
      <table>
        <thead>
          <tr>
            <th>#</th>
            <th>Class</th>
            <SortableTh<ObjRow> label="Shallow" colKey="shallow" sortKey={objSort.sortKey} setSortKey={objSort.setSortKey} />
            <SortableTh<ObjRow> label="Retained" colKey="retained" sortKey={objSort.sortKey} setSortKey={objSort.setSortKey} />
            <SortableTh<ObjRow> label="% Heap" colKey="pct_bp" sortKey={objSort.sortKey} setSortKey={objSort.setSortKey} />
          </tr>
        </thead>
        <tbody>
          {objSort.sorted.map((o, i) => (
            <tr key={i}>
              <td className="num">{i + 1}</td>
              <td>
                <code>{o.display_class}</code>{" "}
              </td>
              <td className="num">{formatBytes(o.shallow)}</td>
              <td className="num" title={fmtExactBytes(o.retained)}>
                {formatBytes(o.retained)}
              </td>
              <td className="num">{pctOf(o.retained, total).toFixed(1)}%</td>
            </tr>
          ))}
        </tbody>
      </table>

      <h3>Biggest Classes</h3>
      <table>
        <thead>
          <tr>
            <th>Class</th>
            <SortableTh<ClassRow> label="Instances" colKey="instances" sortKey={clsSort.sortKey} setSortKey={clsSort.setSortKey} />
            <SortableTh<ClassRow> label="Retained" colKey="retained" sortKey={clsSort.sortKey} setSortKey={clsSort.setSortKey} />
            <th className="num">% Heap</th>
          </tr>
        </thead>
        <tbody>
          {clsSort.sorted.map((c, i) => (
            <tr key={i}>
              <td>
                <span className="copy-cell">
                  <code>{c.pretty_class}</code>
                  <CopyBtn text={c.pretty_class} />
                </span>
              </td>
              <td className="num">{fmtCount(c.instances)}</td>
              <td className="num" title={fmtExactBytes(c.retained)}>
                {formatBytes(c.retained)}
              </td>
              <td className="num">{pctOf(c.retained, total).toFixed(1)}%</td>
            </tr>
          ))}
        </tbody>
      </table>

      {pkgRoot.children.length > 0 && (
        <>
          <h3>Biggest Packages</h3>
          <p className="subtitle">
            Expand a package to drill into its sub-packages. Totals are cumulative over the subtree. Only top-level
            dominators retaining at least {(t.threshold_bp / 100).toFixed(t.threshold_bp % 100 === 0 ? 0 : 2)}% of the
            heap are included (smaller ones are pruned, MAT-style).
          </p>
          <TreemapBar
            root={pkgRoot}
            onSelect={(idx) => document.getElementById(`pkg-${idx}`)?.scrollIntoView({ behavior: "smooth", block: "center" })}
          />
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
function ThreadLocalsTable({ objs }: { objs: ThreadLocalObj[] }) {
  if (objs.length === 0) return null;
  return (
    <details className="thread-locals-detail">
      <summary>Local root objects ({fmtCount(objs.length)})</summary>
      <table>
        <thead>
          <tr>
            <th>Object</th>
            <th className="num">Shallow</th>
            <th className="num">Retained</th>
          </tr>
        </thead>
        <tbody>
          {objs.map((o, i) => (
            <tr key={i}>
              <td>
                <span className="copy-cell">
                  <code>{o.display_class}</code>
                  <CopyBtn text={o.display_class} />
                </span>
              </td>
              <td className="num">{formatBytes(o.shallow)}</td>
              <td className="num">{formatBytes(o.retained)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </details>
  );
}

function ThreadCard({ t, open }: { t: ThreadInfo; open?: boolean }) {
  const cls = t.class_name ?? "<unresolved>";
  const name = t.name?.trim();
  const sig = t.significant_frames ?? [];
  return (
    <details className="thread" open={open} id={`thread-${t.thread_serial}`}>
      <summary>
        {name ? (
          <>
            <span className="thread-name">"{name}"</span>{" "}
            <span className="thread-serial">
              Thread {t.thread_serial}
            </span>{" "}
            (<code>{cls}</code>)
          </>
        ) : (
          <>
            Thread {t.thread_serial} (<code>{cls}</code>)
          </>
        )}
        {" "}— {fmtCount(t.frames.length)} frame
        {t.frames.length === 1 ? "" : "s"}
        {t.local_root_count > 0 ? (
          <>
            {" "}·{" "}
            <span className="thread-locals">
              {fmtCount(t.local_root_count)} local root
              {t.local_root_count === 1 ? "" : "s"}
            </span>
          </>
        ) : null}
      </summary>
      <dl className="thread-props">
        <dt>Shallow Heap</dt>
        <dd>{formatBytes(t.shallow)}</dd>
        <dt>Retained Heap</dt>
        <dd>{formatBytes(t.retained)}</dd>
        <dt>Max. Locals' Retained</dt>
        <dd>{formatBytes(t.max_local_retained)}</dd>
        <dt>Context Class Loader</dt>
        <dd>{t.context_class_loader ? <code>{t.context_class_loader}</code> : "—"}</dd>
        <dt>Is Daemon</dt>
        <dd>{t.is_daemon ? "true" : "false"}</dd>
        <dt>Priority</dt>
        <dd>{t.priority}</dd>
        <dt>State</dt>
        <dd>{t.thread_state || "—"}</dd>
      </dl>
      {t.local_objects && <ThreadLocalsTable objs={t.local_objects} />}
      {sig.length > 0 ? (
        <ul className="sig-frames">
          {sig.map((sf, i) => (
            <li key={i}>
              <code>{sf.frame}</code>
              {sf.locals.length > 0 && (
                <ul>
                  {sf.locals.map((loc, j) => (
                    <li key={j}>
                      <code>{loc.display_class}</code> retains {formatBytes(loc.retained)} (
                      {loc.pct.toFixed(1)}%)
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      ) : (
        <pre className="stack">{t.frames.join("\n")}</pre>
      )}
    </details>
  );
}

// ── Thread Overview table (always-on properties, mirrors MAT columns) ──────────
function ThreadOverviewTable({ threads }: { threads: ThreadInfo[] }) {
  if (threads.length === 0) return null;
  return (
    <details className="thread-overview-detail" open>
      <summary>Thread Overview ({fmtCount(threads.length)})</summary>
      <table>
        <thead>
          <tr>
            <th>Name</th>
            <th className="num">Shallow</th>
            <th className="num">Retained</th>
            <th className="num">Max. Locals' Retained</th>
            <th>Context Class Loader</th>
            <th>Daemon</th>
            <th className="num">Priority</th>
            <th>State</th>
          </tr>
        </thead>
        <tbody>
          {threads.map((t, i) => (
            <tr key={i}>
              <td><a href={`#thread-${t.thread_serial}`}>{t.name?.trim() || `<thread ${t.thread_serial}>`}</a></td>
              <td className="num">{formatBytes(t.shallow)}</td>
              <td className="num">{formatBytes(t.retained)}</td>
              <td className="num">{formatBytes(t.max_local_retained)}</td>
              <td>{t.context_class_loader ? <code>{t.context_class_loader}</code> : "—"}</td>
              <td>{t.is_daemon ? "yes" : "no"}</td>
              <td className="num">{t.priority}</td>
              <td>{t.thread_state || "—"}</td>
            </tr>
          ))}
        </tbody>
      </table>
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
  const [sortKey, setSortKey] = React.useState<ComponentKey>("retained");
  const components = data?.components ?? [];
  const sorted = React.useMemo(
    () => [...components].sort((a, b) => b[sortKey] - a[sortKey]),
    [components, sortKey],
  );
  if (components.length === 0) return null;
  return (
    <section id="top-components">
      <h2>Top Components</h2>
      <p className="subtitle">
        Retained heap grouped by class loader (component); % Heap is the share of total reachable heap.
      </p>
      <details open>
        <summary>Components by retained heap ({fmtCount(components.length)} rows)</summary>
        <table>
          <thead>
            <tr>
              <th>Component</th>
              {COMPONENT_COLS.map((c) => (
                <SortableTh<Component> key={c.key} label={c.label} colKey={c.key} sortKey={sortKey} setSortKey={setSortKey} />
              ))}
              <th>Top classes</th>
            </tr>
          </thead>
          <tbody>
            {sorted.map((c, i) => (
              <tr key={i}>
                <td>
                  <code>{c.loader_label}</code>
                </td>
                <td className="num">{formatBytes(c.retained)}</td>
                <td className="num">{c.pct.toFixed(1)}%</td>
                <td>
                  {c.top_classes.map((cc, j) => (
                    <span key={j}>
                      {j > 0 ? ", " : ""}
                      <code>{cc.pretty_class}</code> ({formatBytes(cc.retained)})
                    </span>
                  ))}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </details>
    </section>
  );
}

// ── Arrays by Size ─────────────────────────────────────────────────────────
// Power-of-two array-length histogram (object vs primitive arrays). Always-on;
// mirrors render_md.rs::render_arrays_by_size.
function ArraysBySizeSection({ data }: { data?: ArraysBySize }) {
  const obj = data?.obj_array_buckets ?? [];
  const prim = data?.prim_array_buckets ?? [];
  const zero = data?.zero_length_count ?? 0;
  const empty = obj.length === 0 && prim.length === 0 && zero === 0;

  const bucketTable = (title: string, buckets: ArraysBySize["obj_array_buckets"]) => (
    <>
      <h3>{title}</h3>
      {buckets.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th className="num">Max length</th>
              <th className="num">Objects</th>
              <th className="num">Shallow</th>
            </tr>
          </thead>
          <tbody>
            {buckets.map((b, i) => (
              <tr key={i}>
                <td className="num">&le; {fmtCount(b.upper_len)}</td>
                <td className="num">{fmtCount(b.objects)}</td>
                <td className="num">{formatBytes(b.shallow)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );

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
  const topArraysBlock = (t: TopArrays | undefined, kind: string) => {
    const individual = t?.top_individual ?? [];
    const byClass = t?.top_by_class ?? [];
    return (
      <>
        <h3>Top Arrays ({kind})</h3>
        <p className="subtitle">
          The largest {kind} arrays by shallow size, individually and aggregated by array class.
        </p>
        {individual.length === 0 ? (
          <p className="subtitle">None.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>Array class</th>
                <th className="num">Length</th>
                <th className="num">Shallow</th>
              </tr>
            </thead>
            <tbody>
              {individual.map((r, i) => (
                <tr key={i}>
                  <td><code>{r.array_class}</code></td>
                  <td className="num">{fmtCount(r.length)}</td>
                  <td className="num">{formatBytes(r.shallow)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <h4>Top Array Classes ({kind})</h4>
        {byClass.length === 0 ? (
          <p className="subtitle">None.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>Array class</th>
                <th className="num">Instances</th>
                <th className="num">Shallow</th>
              </tr>
            </thead>
            <tbody>
              {byClass.map((r, i) => (
                <tr key={i}>
                  <td><code>{r.array_class}</code></td>
                  <td className="num">{fmtCount(r.objects)}</td>
                  <td className="num">{formatBytes(r.shallow)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </>
    );
  };

  // Format a basis-point fill/load range as a percent label (e.g. "0–10%").
  const ratioLabel = (b: FillRatioBucket) => `${b.lower_ratio_bp / 100}–${b.upper_ratio_bp / 100}%`;

  // A fill/wasted table (Collection Fill Ratio, Array Fill Ratio) sharing 4 cols.
  const fillTable = (label: string, itemsHeader: string, buckets: FillRatioBucket[]) => (
    <table>
      <thead>
        <tr>
          <th className="num">{label}</th>
          <th className="num">{itemsHeader}</th>
          <th className="num">Shallow</th>
          <th className="num">Wasted</th>
        </tr>
      </thead>
      <tbody>
        {buckets.map((b, i) => (
          <tr key={i}>
            <td className="num">{ratioLabel(b)}</td>
            <td className="num">{fmtCount(b.objects)}</td>
            <td className="num">{formatBytes(b.shallow)}</td>
            <td className="num">{formatBytes(b.wasted)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );

  const cfrBuckets = cfr?.buckets ?? [];
  const cbsBuckets = cbs?.buckets ?? [];
  const afrBuckets = afr?.buckets ?? [];
  const mcrBuckets = mcr?.buckets ?? [];
  const cpaRows = cpa?.rows ?? [];
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
      ) : (
        <table>
          <thead>
            <tr>
              <th>Kind</th>
              <th className="num">Count</th>
              <th className="num">Total Elements</th>
              <th className="num">Max Elements</th>
              <th className="num">Total Shallow</th>
            </tr>
          </thead>
          <tbody>
            {kindRows.map((s, i) => (
              <tr key={i}>
                <td>{s.kind}</td>
                <td className="num">{fmtCount(s.count)}</td>
                <td className="num">{fmtCount(s.total_elements)}</td>
                <td className="num">{fmtCount(s.max_elements)}</td>
                <td className="num">{formatBytes(s.total_shallow)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <h3>Collection Fill Ratio</h3>
      <p className="subtitle">
        {fmtCount(cfr?.tracked ?? 0)} tracked of {fmtCount(cfr?.total ?? 0)} collections.
      </p>
      {cfrBuckets.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        fillTable("Fill %", "Collections", cfrBuckets)
      )}

      <h3>Collections by Size</h3>
      <p className="subtitle">
        {fmtCount(cbs?.tracked ?? 0)} tracked; {fmtCount(cbs?.empty_count ?? 0)} empty.
      </p>
      {cbsBuckets.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th className="num">Size &le;</th>
              <th className="num">Collections</th>
              <th className="num">Shallow</th>
            </tr>
          </thead>
          <tbody>
            {cbsBuckets.map((b, i) => (
              <tr key={i}>
                <td className="num">&le; {fmtCount(b.upper_len)}</td>
                <td className="num">{fmtCount(b.objects)}</td>
                <td className="num">{formatBytes(b.shallow)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <h3>Array Fill Ratio</h3>
      <p className="subtitle">{fmtCount(afr?.tracked ?? 0)} tracked object arrays.</p>
      {afrBuckets.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        fillTable("Fill %", "Arrays", afrBuckets)
      )}

      <h3>Map Collision Ratio</h3>
      <p className="subtitle">
        {fmtCount(mcr?.tracked ?? 0)} tracked of {fmtCount(mcr?.total ?? 0)} maps (occupied slots ÷ size; lower is
        worse).
      </p>
      {mcrBuckets.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th className="num">Load %</th>
              <th className="num">Maps</th>
              <th className="num">Shallow</th>
            </tr>
          </thead>
          <tbody>
            {mcrBuckets.map((b, i) => (
              <tr key={i}>
                <td className="num">{ratioLabel(b)}</td>
                <td className="num">{fmtCount(b.objects)}</td>
                <td className="num">{formatBytes(b.shallow)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <h3>Constant Primitive Arrays</h3>
      <p className="subtitle">
        Primitive arrays whose every element is identical.
        {cpa?.truncated ? " (list truncated; remaining groups folded into one row)." : ""}
      </p>
      {cpaRows.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Array class</th>
              <th className="num">Length</th>
              <th className="num">Value</th>
              <th className="num">Objects</th>
              <th className="num">Shallow</th>
            </tr>
          </thead>
          <tbody>
            {cpaRows.map((r, i) => (
              <tr key={i}>
                <td><code>{r.array_class}</code></td>
                <td className="num">{fmtCount(r.length)}</td>
                <td className="num">{String(r.value)}</td>
                <td className="num">{fmtCount(r.objects)}</td>
                <td className="num">{formatBytes(r.shallow)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {topArraysBlock(topPrim, "primitive")}
      {topArraysBlock(topObj, "object")}
    </section>
  );
}

// ── Container Attribution (Class#field) ──────────────────────────────────────
// Which holder Class#field points at the most container memory. Absent when
// --collections was off (data undefined → section not rendered). Mirrors
// render_md.rs::render_collection_attribution (HTML has no bar columns).
function CollectionAttributionSection({ data }: { data?: CollectionAttribution }) {
  if (!data) return null;
  const mostOverall = data.most_overall ?? [];
  const biggestSingle = data.biggest_single ?? [];
  return (
    <section id="container-attribution">
      <h2>Container Attribution (Class#field)</h2>
      <p className="subtitle">
        Which holder Class#field points at the most container memory. Two rankings: total across
        all containers reached through a field, and the single largest container per field.
      </p>

      <h3>Most Overall</h3>
      {mostOverall.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Class#field</th>
              <th>Kind</th>
              <th className="num">Containers</th>
              <th className="num">Holder Instances</th>
              <th className="num">Total Elements</th>
              <th className="num">Total Retained</th>
            </tr>
          </thead>
          <tbody>
            {mostOverall.map((r, i) => (
              <tr key={i}>
                <td><code>{r.holder_class}#{r.field}</code></td>
                <td>{r.container_kind}</td>
                <td className="num">{fmtCount(r.container_count)}</td>
                <td className="num">{fmtCount(r.holder_instances)}</td>
                <td className="num">{fmtCount(r.total_elements)}</td>
                <td className="num">{formatBytes(r.total_retained)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <h3>Biggest Single</h3>
      {biggestSingle.length === 0 ? (
        <p className="subtitle">None.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Class#field</th>
              <th>Container Class</th>
              <th className="num">Elements</th>
              <th className="num">Capacity</th>
              <th className="num">Retained</th>
            </tr>
          </thead>
          <tbody>
            {biggestSingle.map((r, i) => (
              <tr key={i}>
                <td><code>{r.holder_class}#{r.field}</code></td>
                <td><code>{r.container_class}</code></td>
                <td className="num">{fmtCount(r.elements)}</td>
                <td className="num">{fmtCount(r.capacity)}</td>
                <td className="num">{formatBytes(r.retained)}</td>
              </tr>
            ))}
          </tbody>
        </table>
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

// ── References ──────────────────────────────────────────────────────────────
// Soft/weak/phantom reference referents (what they point at). Always-on;
// mirrors render_md.rs::render_references.
function ReferencesSection({ data }: { data?: ReferencesAnalysis }) {
  const kinds: ReferenceStats[] = [data?.soft, data?.weak, data?.phantom].filter(
    (s): s is ReferenceStats => s != null,
  );

  const classTable = (rows: RefStatClassRow[]) => (
    <table>
      <thead>
        <tr>
          <th>Class</th>
          <th className="num">Objects</th>
          <th className="num">Shallow</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((r, i) => (
          <tr key={i}>
            <td><code>{r.pretty_class}</code></td>
            <td className="num">{fmtCount(r.objects)}</td>
            <td className="num">{formatBytes(r.shallow)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );

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
            <p className="subtitle">{fmtCount(stats.reference_instances)} reference instances.</p>
            <h4>Referent classes</h4>
            {classTable(stats.referent_histogram ?? [])}
            {(stats.only_weakly_retained ?? []).length > 0 && (
              <>
                <h4>Only-weakly retained (approximate)</h4>
                {classTable(stats.only_weakly_retained)}
              </>
            )}
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
      ) : (
        <table>
          <thead>
            <tr>
              <th>Object</th>
              <th className="num">Retained</th>
              <th>Largest Child</th>
              <th className="num">Child Retained</th>
              <th className="num">Drop</th>
            </tr>
          </thead>
          <tbody>
            {drops.map((r, i) => (
              <tr key={i}>
                <td><span className="copy-cell"><code>{r.display_class}</code><CopyBtn text={r.display_class} /></span></td>
                <td className="num">{formatBytes(r.retained)}</td>
                <td>{r.largest_child_class ? <code>{r.largest_child_class}</code> : "—"}</td>
                <td className="num">{formatBytes(r.largest_child_retained)}</td>
                <td className="num">{formatBytes(r.drop_bytes)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <h3>Immediate Dominators</h3>
      <p className="subtitle">
        Objects immediately dominated, rolled up by the dominator's class; a heavy dominated shallow heap under one
        class flags a retention hub.
      </p>
      {idoms.length === 0 ? (
        <p className="subtitle">No immediate dominators.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Dominator Class</th>
              <th className="num">#Dominators</th>
              <th className="num">#Dominated</th>
              <th className="num">Dominator Shallow</th>
              <th className="num">Dominated Shallow</th>
            </tr>
          </thead>
          <tbody>
            {idoms.map((r, i) => (
              <tr key={i}>
                <td><span className="copy-cell"><code>{r.dominator_class}</code><CopyBtn text={r.dominator_class} /></span></td>
                <td className="num">{fmtCount(r.dominator_count)}</td>
                <td className="num">{fmtCount(r.dominated_count)}</td>
                <td className="num">{formatBytes(r.dominator_shallow)}</td>
                <td className="num">{formatBytes(r.dominated_shallow)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

// ── Unreachable Objects ─────────────────────────────────────────────────────
// Per-class histogram of objects not dominated by the virtual root
// (idom == u32::MAX). Always-on; mirrors render_md.rs::render_unreachable_histogram.
type UnreachableKey = "objects" | "shallow";
const UNREACHABLE_COLS: { key: UnreachableKey; label: string }[] = [
  { key: "objects", label: "Objects" },
  { key: "shallow", label: "Shallow" },
];

function UnreachableObjectsSection({ data }: { data?: SystemOverview }) {
  const rows: UnreachableClassRow[] = data?.unreachable_histogram ?? [];
  const [sortKey, setSortKey] = React.useState<UnreachableKey>("shallow");
  const sorted = React.useMemo(() => [...rows].sort((a, b) => b[sortKey] - a[sortKey]), [rows, sortKey]);
  return (
    <section id="unreachable-objects">
      <h2>Unreachable Objects</h2>
      {rows.length === 0 ? (
        <p className="subtitle">No unreachable objects.</p>
      ) : (
        <>
          <p className="subtitle">
            {fmtCount(data?.unreachable_count ?? 0)} unreachable objects retaining{" "}
            {formatBytes(data?.unreachable_shallow ?? 0)} shallow (top {fmtCount(rows.length)} classes by shallow).
          </p>
          <details open>
            <summary>Unreachable objects by class ({fmtCount(rows.length)} rows)</summary>
            <table>
              <thead>
                <tr>
                  <th>Class</th>
                  {UNREACHABLE_COLS.map((c) => (
                    <SortableTh<UnreachableClassRow> key={c.key} label={c.label} colKey={c.key} sortKey={sortKey} setSortKey={setSortKey} />
                  ))}
                </tr>
              </thead>
              <tbody>
                {sorted.map((r, i) => (
                  <tr key={i}>
                    <td><code>{r.pretty_class}</code></td>
                    <td className="num">{fmtCount(r.objects)}</td>
                    <td className="num">{formatBytes(r.shallow)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
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
  return (
    <section id="alloc-sites">
      <h2>Allocation Sites</h2>
      <p className="subtitle">Objects grouped by the stack trace that allocated them.</p>
      {!data.traces_present ? (
        <p className="subtitle">
          Allocation tracking was off in this dump (stack_trace_serial = 0); no allocation sites available.
        </p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>Stack</th>
              <th className="num">Objects</th>
              <th className="num">Shallow</th>
              <th className="num">Retained</th>
            </tr>
          </thead>
          <tbody>
            {data.sites.map((s, i) => (
              <tr key={i}>
                <td>
                  {s.frames.length > 0 ? (
                    <code>{s.frames[0]}</code>
                  ) : (
                    <span className="hint">serial {s.stack_serial}</span>
                  )}
                </td>
                <td className="num">{fmtCount(s.object_count)}</td>
                <td className="num">{formatBytes(s.shallow_total)}</td>
                <td className="num" title={fmtExactBytes(s.retained_total)}>
                  {formatBytes(s.retained_total)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

// ── Retention Concentration ─────────────────────────────────────────────────
// How much of the heap the few biggest top-level dominators hold. Mirrors
// render_md.rs::render_retention_concentration.
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
      <table>
        <thead>
          <tr>
            <th>Scope</th>
            <th className="num">Retained Share</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td>Top 1 object</td>
            <td className="num">{(rc.top1_bp / 100).toFixed(1)}%</td>
          </tr>
          <tr>
            <td>Top 10 objects</td>
            <td className="num">{(rc.top10_bp / 100).toFixed(1)}%</td>
          </tr>
          <tr>
            <td>Top 100 objects</td>
            <td className="num">{(rc.top100_bp / 100).toFixed(1)}%</td>
          </tr>
          <tr>
            <td>Objects each &ge;1%</td>
            <td className="num">{fmtCount(rc.num_objects_ge_1pct)}</td>
          </tr>
        </tbody>
      </table>
    </section>
  );
}

// ── Dominator-Depth Distribution ─────────────────────────────────────────────
// Objects per idom-hop below a GC root. Mirrors render_md.rs::render_dominator_depth.
function DominatorDepthSection({ report }: { report: Report }) {
  const hist = report.overview.dominator_depth_histogram;
  if (!hist || hist.length === 0) return null;

  const totalObjs = hist.reduce((s, b) => s + b.objects, 0);
  const maxDepth = hist.reduce((m, b) => Math.max(m, b.depth), 0);

  // Compute cumulative percentage for each bucket.
  type DepthRow = { depth: number; objects: number; pct: number; cum: number };
  const rows: DepthRow[] = [];
  let cumSum = 0;
  for (const b of hist) {
    cumSum += b.objects;
    rows.push({
      depth: b.depth,
      objects: b.objects,
      pct: totalObjs > 0 ? (b.objects / totalObjs) * 100 : 0,
      cum: totalObjs > 0 ? (cumSum / totalObjs) * 100 : 0,
    });
  }

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
        <table>
          <thead>
            <tr>
              <th className="num">Depth</th>
              <th className="num">Objects</th>
              <th className="num">% Objects</th>
              <th className="num">Cumulative %</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r, i) => (
              <tr key={i}>
                <td className="num">{r.depth}</td>
                <td className="num">{fmtCount(r.objects)}</td>
                <td className="num">{r.pct.toFixed(2)}%</td>
                <td className="num">{r.cum.toFixed(2)}%</td>
              </tr>
            ))}
          </tbody>
        </table>
      </details>
    </section>
  );
}

// ── Leak Indicators ─────────────────────────────────────────────────────────
// Scalar signals for common Java leak patterns. Only rendered when at least
// one indicator is non-zero. Mirrors render_md.rs::render_leak_indicators.
function LeakIndicatorsSection({ data }: { data?: LeakIndicators }) {
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
      <table>
        <thead>
          <tr>
            <th>Indicator</th>
            <th className="num">Value</th>
          </tr>
        </thead>
        <tbody>
          {anonymous_class_count > 0 && (
            <tr>
              <td>Anonymous/generated classes</td>
              <td className="num">{fmtCount(anonymous_class_count)}</td>
            </tr>
          )}
          {thread_local_null_key_count > 0 && (
            <tr>
              <td>ThreadLocal null-key entries (cleared referent)</td>
              <td className="num">{fmtCount(thread_local_null_key_count)}</td>
            </tr>
          )}
          {direct_byte_buffer_capacity_sum > 0 && (
            <tr>
              <td>DirectByteBuffer total capacity</td>
              <td className="num">{formatBytes(direct_byte_buffer_capacity_sum)}</td>
            </tr>
          )}
        </tbody>
      </table>
    </section>
  );
}

// ── Glossary (end section, mirrors the Markdown glossary) ─────────────────────
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
function fmtDeltaBytes(n: number): string {
  if (n === 0) return "0 B";
  const sign = n > 0 ? "+" : MINUS;
  return sign + formatBytes(Math.abs(n));
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
  const n = labels.length;
  // Sort key: -1 = Δ column (default), 0..n-1 = a per-report retained column.
  const [sortCol, setSortCol] = React.useState<number>(-1);
  const sorted = React.useMemo(() => {
    const keyed = [...rows];
    keyed.sort((a, b) => {
      const av = sortCol < 0 ? a.delta_retained : (a.retained[sortCol] ?? 0);
      const bv = sortCol < 0 ? b.delta_retained : (b.retained[sortCol] ?? 0);
      if (bv !== av) return bv - av;
      return a.pretty_class.localeCompare(b.pretty_class);
    });
    return keyed;
  }, [rows, sortCol]);

  const th = (label: string, col: number, title: string) => {
    const active = sortCol === col;
    return (
      <th
        key={col}
        className={"num sortable" + (active ? " active" : "")}
        onClick={() => setSortCol(col)}
        title={`Sort by ${title} (descending)`}
      >
        {label} {active ? "▾" : ""}
      </th>
    );
  };

  return (
    <table className="data">
      <thead>
        <tr>
          <th>{nameLabel}</th>
          {labels.map((lbl, i) => th(`r${i + 1}`, i, `${lbl} retained`))}
          {th("Δ(r1→rN)", -1, "Δ retained (first→last)")}
          {showNew ? <th>New?</th> : null}
        </tr>
      </thead>
      <tbody>
        {sorted.map((row) => (
          <tr key={row.pretty_class}>
            <td><code>{row.pretty_class}</code></td>
            {Array.from({ length: n }, (_, i) => (
              <td key={i} className="num">{formatBytes(row.retained[i] ?? 0)}</td>
            ))}
            <td className="num">{fmtDeltaBytes(row.delta_retained)}</td>
            {showNew ? (
              <td>{"is_new" in row && row.is_new ? "yes" : ""}</td>
            ) : null}
          </tr>
        ))}
      </tbody>
    </table>
  );
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
function diffVerdict(diff: SeriesDiffResult): string {
  const firstShallow = diff.total_shallow[0] ?? 0;
  const pct = firstShallow > 0 ? (diff.delta_total_shallow / firstShallow) * 100 : 0;
  const newSuspects = diff.grown_suspects.filter((s) => s.is_new).length;
  let line: string;
  if (diff.delta_total_shallow > 0) {
    const lead = diff.growth_leaders[0];
    const driver = lead
      ? `; largest driver ${lead.pretty_class} (${fmtDeltaBytes(lead.delta_retained)} retained)`
      : "";
    line = `Heap grew ${pct.toFixed(1)}% (${fmtDeltaBytes(diff.delta_total_shallow)} shallow)${driver}.`;
  } else if (diff.delta_total_shallow < 0) {
    line = `Heap shrank ${Math.abs(pct).toFixed(1)}% (${fmtDeltaBytes(diff.delta_total_shallow)} shallow); no net growth.`;
  } else {
    line = "Heap size is unchanged.";
  }
  if (newSuspects > 0) {
    line += ` ${newSuspects} new suspect${newSuspects === 1 ? "" : "s"}.`;
  }
  return line;
}

export function DiffApp({ diff }: { diff: SeriesDiffResult }) {
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
        <p><strong>Verdict:</strong> {diffVerdict(diff)}</p>
        <ul>
          <li><strong>Δ Objects (r1→rN):</strong> {fmtDeltaCount(diff.delta_total_objects)}</li>
          <li><strong>Δ Shallow heap (r1→rN):</strong> {fmtDeltaBytes(diff.delta_total_shallow)}</li>
          <li><strong>Net Δ Retained (all classes, r1→rN):</strong> {fmtDeltaBytes(diff.net_delta_retained)}</li>
        </ul>
      </section>

      <DiffSection
        title="Growth Leaders (by Δ retained)"
        nameLabel="Class"
        labels={labels}
        rows={diff.growth_leaders}
        emptyNote="No class grew in retained heap."
      />
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
      <DiffSection
        title="Disappeared Leak Suspects"
        nameLabel="Suspect"
        labels={labels}
        rows={diff.gone_suspects}
        emptyNote="No leak suspect disappeared in the current dump."
      />
      <BackToTop />
    </div>
  );
}

export default function App({ report }: { report: Report }) {
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
    <div className="app">
      <a href="#triage" className="skip-link">Skip to content</a>
      <h1>
        Heap Dump Analysis: <code>{report.overview.source_name}</code>
      </h1>
      <p className="subtitle">Generated by hprof-analyzer — {report.generated}</p>
      <div className="theme-toggle-wrap">
        <ThemeToggle />
      </div>
      <Nav report={report} />
      <OomTriage report={report} />
      <KpiStrip report={report} />
      <SystemOverviewSection report={report} />
      <RecordCensusSection report={report} />
      <LeakSuspectsSection report={report} />
      <TopConsumersSection report={report} />
      <SizeDistributionSection report={report} />
      <DuplicateStringsSection report={report} />
      <DominatorAnalysisSection data={report.dominator_analysis} />
      <ThreadsSection report={report} />
      {report.top_components?.components?.length ? (
        <TopComponentsSection data={report.top_components} />
      ) : null}
      <ArraysBySizeSection data={report.arrays_by_size} />
      <CollectionsSection data={report.collections} />
      {report.collection_attribution && (
        <CollectionAttributionSection data={report.collection_attribution} />
      )}
      <ReferencesSection data={report.references} />
      <UnreachableObjectsSection data={report.overview} />
      {report.alloc_sites && <AllocSitesSection data={report.alloc_sites} />}
      <RetentionConcentrationSection report={report} />
      <DominatorDepthSection report={report} />
      <LeakIndicatorsSection data={report.leak_indicators} />
      <GlossarySection />
      <BackToTop />
    </div>
  );
}
