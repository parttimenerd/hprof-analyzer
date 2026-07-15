import React from "react";
import type { AllocSites, ClassRow, DomTreeNode, HistRow, ObjRow, PackageNode, Report, RootPathStep, Suspect, ThreadInfo, ThreadLocalObj, TopComponents } from "./types";
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
  const items: [string, string][] = [
    ["triage", "OOM Triage"],
    ["overview", "System Overview"],
    ["leaks", "Leak Suspects"],
    ["top", "Top Consumers"],
    ["threads", "Threads"],
  ];
  // show the entry only when the field is present.
  if (report.top_components?.components?.length) items.push(["top-components", "Top Components"]);
  if (report.alloc_sites) items.push(["alloc-sites", "Allocation Sites"]);
  const rc = report.overview.retention_concentration;
  if (rc.top1_bp > 0 || rc.num_objects_ge_1pct > 0) {
    items.push(["retention-concentration", "Retention Concentration"]);
  }
  if (report.overview.dominator_depth_histogram.length > 0) {
    items.push(["dominator-depth-distribution", "Dominator-Depth Distribution"]);
  }
  items.push(["glossary", "Glossary"]);

  const [active, setActive] = React.useState<string>("");

  React.useEffect(() => {
    // rootMargin shrinks the detection zone to the top-center of the viewport
    // so a section activates when it's clearly in focus, not just barely on-screen.
    const observer = new IntersectionObserver(
      (entries) => {
        // Update a shared map of which sections are intersecting.
        entries.forEach((e) => {
          intersecting.set(e.target.id, e.isIntersecting);
        });
        // Pick the topmost intersecting section; if none, pick the last one above fold.
        const ids = items.map(([id]) => id);
        let chosen = "";
        let lowestAbove = -Infinity;
        for (const id of ids) {
          const el = document.getElementById(id);
          if (!el) continue;
          const top = el.getBoundingClientRect().top;
          if (intersecting.get(id)) {
            chosen = id;
            break; // items are in DOM order; first intersecting = topmost
          }
          if (top < 0 && top > lowestAbove) {
            lowestAbove = top;
            chosen = id;
          }
        }
        setActive(chosen);
      },
      { rootMargin: "-40% 0px -55% 0px" },
    );

    const intersecting = new Map<string, boolean>();
    items.forEach(([id]) => {
      const el = document.getElementById(id);
      if (el) observer.observe(el);
    });
    return () => observer.disconnect();
  }, []); // ids are static — runs once on mount

  return (
    <nav className="toc">
      {items.map(([id, label]) => (
        <a key={id} href={`#${id}`} className={id === active ? "active" : ""}>
          {label}
        </a>
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
    <div className="oom" id="triage">
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

  return (
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
        <div className="kpi-value" title={dominantClass}>
          <code>{dominantClass}</code>
        </div>
        <div className="kpi-label">Dominant retainer</div>
      </div>
      <div className="kpi">
        <div className="kpi-value">{fmtCount(report.overview.gc_roots)}</div>
        <div className="kpi-label">GC roots</div>
      </div>
    </div>
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
type HistKey = "instances" | "shallow" | "retained";
const HIST_COLS: { key: HistKey; label: string }[] = [
  { key: "instances", label: "Instances" },
  { key: "shallow", label: "Shallow" },
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
                <code>{h.pretty_class}</code>
              </td>
              {showLoader && (
                <td>
                  <LoaderCell label={h.loader_label} />
                </td>
              )}
              <td className="num">{fmtCount(h.instances)}</td>
              <td className="num">{formatBytes(h.shallow)}</td>
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

// ── ChartOrNote ──────────────────────────────────────────────────────────────
// Renders children when hasData is true; otherwise shows a muted note matching
// the "System properties not captured in this dump." pattern.
function ChartOrNote({ hasData, note, children }: { hasData: boolean; note: string; children: React.ReactNode }) {
  if (!hasData) return <p className="subtitle" style={{ color: "var(--muted)" }}>{note}</p>;
  return <>{children}</>;
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
                <tr key={i}>
                  <td title={d.loaders.join(", ")}>
                    <code>{d.pretty_class}</code>
                  </td>
                  <td className="num">{fmtCount(d.loader_count)}</td>
                  <td className="num">{fmtCount(d.total_instances)}</td>
                  <td className="num">{formatBytes(d.total_retained)}</td>
                </tr>
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
                <code>{c.pretty_class}</code>
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
                <code>{o.display_class}</code>
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
    <details className="thread" open={open}>
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
              <td>{t.name?.trim() || `<thread ${t.thread_serial}>`}</td>
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
function TopComponentsSection({ data }: { data: TopComponents }) {
  if (!data?.components?.length) return null;
  return (
    <section id="top-components">
      <h2>Top Components</h2>
      <p className="subtitle">
        Retained heap grouped by class loader (component); % Heap is the share of total reachable heap.
      </p>
      <table>
        <thead>
          <tr>
            <th>Component</th>
            <th className="num">Retained</th>
            <th className="num">% Heap</th>
            <th>Top classes</th>
          </tr>
        </thead>
        <tbody>
          {data.components.map((c, i) => (
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
        <p className="subtitle" style={{ color: "var(--muted)" }}>
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

export default function App({ report }: { report: Report }) {
  return (
    <div className="app">
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
      <LeakSuspectsSection report={report} />
      <TopConsumersSection report={report} />
      <ThreadsSection report={report} />
      {report.top_components?.components?.length ? (
        <TopComponentsSection data={report.top_components} />
      ) : null}
      {report.alloc_sites && <AllocSitesSection data={report.alloc_sites} />}
      <RetentionConcentrationSection report={report} />
      <DominatorDepthSection report={report} />
      <GlossarySection />
      <BackToTop />
    </div>
  );
}
